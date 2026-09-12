//! Fallback worker backend seam and Phase-0 CPU frame currency.
//!
//! [`FallbackBackend`] abstracts the offscreen renderer (a future CDP-driven
//! headless-Chromium driver, or [`crate::testing::FakeBackend`] in tests).
//! Frames travel as plain [`CpuFrame`] RGBA images; the Phase-3 zero-copy GPU
//! texture importers replace this currency without touching supervision.

use std::fmt;
use std::sync::Arc;

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
    /// Shared so clones (supervisor handoff, test doubles) and paint-time
    /// uploads pass the allocation by reference instead of re-copying it.
    rgba: Arc<Vec<u8>>,
}

impl CpuFrame {
    /// Total RGBA byte count for `width` x `height`; `None` on overflow.
    fn byte_len(width: u32, height: u32) -> Option<usize> {
        (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)
    }

    /// A solid-color frame; `None` when the dimensions overflow `usize` or
    /// the backing store cannot be allocated (a hostile geometry must fail
    /// validation, not attempt a multi-exabyte allocation or abort on OOM).
    pub fn solid(width: u32, height: u32, pixel: [u8; 4]) -> Option<Self> {
        let bytes = Self::byte_len(width, height)?;
        let mut rgba = Vec::new();
        rgba.try_reserve_exact(bytes).ok()?;
        for _ in 0..bytes / 4 {
            rgba.extend_from_slice(&pixel);
        }
        Some(Self {
            width,
            height,
            rgba: Arc::new(rgba),
        })
    }

    /// Row-major `Rgba8` bytes; `None` unless `rgba.len() == 4 * width * height`.
    ///
    /// The size check is overflow-safe: dimensions whose product wraps
    /// `usize` never validate, so a crafted `(width, height, len)` triple
    /// cannot construct a frame whose claimed geometry mismatches its buffer.
    pub fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Option<Self> {
        (rgba.len() == Self::byte_len(width, height)?).then_some(Self {
            width,
            height,
            rgba: Arc::new(rgba),
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

    /// Share ownership of the pixel allocation: paint-time uploads and frame
    /// handoffs clone the `Arc` instead of copying the whole image.
    pub fn shared_rgba(&self) -> Arc<Vec<u8>> {
        Arc::clone(&self.rgba)
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
    /// Purge GPU/memory caches and drop cached frames: afterwards [`FallbackBackend::frame`]
    /// must return `None` until a fresh post-wake frame arrives.
    fn hibernate(&mut self);
    /// End the worker and reclaim all of its memory.
    fn terminate(&mut self);
    /// Latest composited frame, if the worker has one.
    ///
    /// Freshness contract: returns `None` unless the worker holds a frame
    /// composited after the most recent spawn/wake. Serving the last
    /// pre-hibernate bitmap post-wake composites stale content as if live.
    fn frame(&mut self) -> Option<CpuFrame>;
}

#[test]
fn solid_frame_has_expected_rgba_layout() {
    let frame = CpuFrame::solid(2, 1, [255, 0, 0, 255]).expect("tiny frame cannot overflow");
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
fn from_rgba_rejects_overflowing_dimensions() {
    // 4 * 2^31 * 2^31 wraps to 0 on 64-bit release (panics in debug): an
    // empty buffer must never validate against giant geometry.
    assert!(CpuFrame::from_rgba(1 << 31, 1 << 31, Vec::new()).is_none());
}

#[test]
fn solid_rejects_overflowing_dimensions() {
    // Same hostile geometry as above: must fail validation, not attempt a
    // multi-exabyte allocation.
    assert!(CpuFrame::solid(1 << 31, 1 << 31, [0, 0, 0, 255]).is_none());
}

#[test]
fn cloned_frames_share_one_pixel_allocation() {
    let frame = CpuFrame::solid(2, 2, [1, 2, 3, 4]).expect("tiny frame cannot overflow");
    let clone = frame.clone();
    assert!(
        Arc::ptr_eq(&frame.shared_rgba(), &clone.shared_rgba()),
        "clones must share the pixel allocation (paint uploads must not copy per frame)"
    );
}

#[test]
fn from_rgba_accepts_exact_length() {
    let rgba = vec![1u8; 4 * 3 * 2];
    let frame = CpuFrame::from_rgba(3, 2, rgba.clone()).expect("exact length must parse");
    assert_eq!((frame.width(), frame.height()), (3, 2));
    assert_eq!(frame.rgba(), rgba.as_slice());
}
