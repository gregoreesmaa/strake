//! Fallback worker backend seam and Phase-0 CPU frame currency.
//!
//! [`FallbackBackend`] abstracts the offscreen renderer (a future CDP-driven
//! headless-Chromium driver, or [`crate::testing::FakeBackend`] in tests).
//! Frames travel as plain [`CpuFrame`] RGBA images; the Phase-3 zero-copy GPU
//! texture importers replace this currency without touching supervision.

use std::fmt;

/// What to render the fallback worker for: the offscreen target's URL and
/// backing-store geometry.
#[derive(Debug, Clone, PartialEq)]
pub struct SpawnSpec {
    /// Document/element URL the worker renders.
    pub url: String,
    /// Backing-store width in physical pixels.
    pub width: u32,
    /// Backing-store height in physical pixels.
    pub height: u32,
    /// Device pixel ratio of the backing store.
    pub scale: f64,
}

/// Typed supervision failures (hand-rolled: no new external error crates).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorError {
    /// The backend refused to spawn a worker.
    SpawnFailed(String),
    /// Operation referenced a surface id that does not exist.
    UnknownSurface(u64),
}

impl fmt::Display for SupervisorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SpawnFailed(message) => write!(f, "fallback worker spawn failed: {message}"),
            Self::UnknownSurface(id) => write!(f, "unknown fallback surface: {id}"),
        }
    }
}

impl std::error::Error for SupervisorError {}

/// A Phase-0 CPU fallback frame: row-major `Rgba8` pixels.
///
/// This is the MVP compositing currency (issue #5, Phase 0): the worker's
/// offscreen output shared as a plain image. Phase 3 replaces it with
/// zero-copy GPU texture imports behind the same widget seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuFrame {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl CpuFrame {
    /// A solid-color frame.
    pub fn solid(width: u32, height: u32, pixel: [u8; 4]) -> Self {
        Self {
            width,
            height,
            rgba: (0..width as usize * height as usize)
                .flat_map(|_| pixel)
                .collect(),
        }
    }

    /// Row-major `Rgba8` bytes; `None` unless `rgba.len() == 4 * width * height`.
    pub fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Option<Self> {
        (rgba.len() == 4 * width as usize * height as usize).then_some(Self {
            width,
            height,
            rgba,
        })
    }

    /// Backing-store width in physical pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Backing-store height in physical pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Row-major `Rgba8` pixel bytes.
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }
}

/// Offscreen fallback renderer driven by [`crate::Supervisor`].
///
/// The supervisor calls `spawn` at most once per worker lifetime (plus once
/// per respawn after termination); `hibernate`/`wake` bracket occlusion;
/// `terminate` ends the worker. Every method must be cheap and non-blocking:
/// supervision policy runs on the embedder's UI thread.
pub trait FallbackBackend {
    /// Start the worker for `spec`. Called lazily on first demand only.
    fn spawn(&mut self, spec: &SpawnSpec) -> Result<(), String>;
    /// Resume a hibernated worker (caches rebuild lazily).
    fn wake(&mut self);
    /// Purge GPU/memory caches; the worker keeps no fresh frames.
    fn hibernate(&mut self);
    /// End the worker and reclaim all of its memory.
    fn terminate(&mut self);
    /// Latest composited frame, if the worker has one.
    fn frame(&mut self) -> Option<CpuFrame>;
}

#[test]
fn solid_frame_has_expected_rgba_layout() {
    let frame = CpuFrame::solid(2, 1, [255, 0, 0, 255]);
    assert_eq!(frame.width(), 2);
    assert_eq!(frame.height(), 1);
    assert_eq!(
        frame.rgba(),
        &[255, 0, 0, 255, 255, 0, 0, 255],
        "row-major RGBA, one pixel per 4 bytes"
    );
}

#[test]
fn from_rgba_rejects_length_mismatch() {
    assert!(CpuFrame::from_rgba(2, 2, vec![0u8; 15]).is_none());
    assert!(CpuFrame::from_rgba(2, 2, vec![0u8; 17]).is_none());
}

#[test]
fn from_rgba_accepts_exact_length() {
    let rgba = vec![1u8; 4 * 3 * 2];
    let frame = CpuFrame::from_rgba(3, 2, rgba.clone()).expect("exact length must parse");
    assert_eq!((frame.width(), frame.height()), (3, 2));
    assert_eq!(frame.rgba(), rgba.as_slice());
}
