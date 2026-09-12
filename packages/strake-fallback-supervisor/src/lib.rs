//! On-demand fallback worker lifecycle for Strake (issue #5, Phase 0).
//!
//! Native-first rendering covers typical web surfaces; the long tail
//! (WebRTC, Widevine DRM, complex iframes) falls back to an offscreen
//! headless-Chromium worker. This crate owns that worker's lifecycle under a
//! hard resource invariant:
//!
//! * **Zero cost at rest** — no worker exists until the first fallback
//!   surface is added; [`Supervisor::new`] performs no spawning.
//! * **Aggressive eviction** — surfaces occluded or idle for
//!   [`HIBERNATE_AFTER`] hibernate the worker (GPU/memory caches purged);
//!   zero live surfaces for [`TERMINATE_AFTER`] terminates it completely.
//! * **Phase-0 compositing** — frames travel as plain [`CpuFrame`] RGBA
//!   images painted through [`FallbackWidget`]. The Phase-3 zero-copy GPU
//!   texture importers (`IOSurface` / `DXGI` / `dma-buf`) plug in behind the
//!   same [`FallbackBackend`] / [`Widget`](strake_dom::Widget) seam.
//!
//! Time is passed in by the embedder as a [`Duration`] so policy is fully
//! deterministic under test (no sleeping, no wall clock).

mod backend;
mod supervisor;
pub mod testing;
mod widget;

pub use backend::{CpuFrame, FallbackBackend, SpawnSpec, SupervisorError};
pub use supervisor::{HIBERNATE_AFTER, Supervisor, TERMINATE_AFTER, WorkerState};
pub use widget::FallbackWidget;
