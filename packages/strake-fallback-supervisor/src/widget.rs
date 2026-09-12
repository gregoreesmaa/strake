//! [`FallbackWidget`]: a [`Widget`](strake_dom::Widget) compositing Phase-0
//! CPU fallback frames into the native paint scene.
//!
//! The embedder loop is: [`Supervisor`](crate::Supervisor) supervises the
//! worker → [`Supervisor::pump_frame`](crate::Supervisor::pump_frame) yields
//! the latest [`CpuFrame`](crate::CpuFrame) → [`FallbackWidget::set_frame`]
//! presents it on the fallback element's box. Phase 3 keeps this seam and
//! swaps the image upload for zero-copy GPU texture imports.

use anyrender::{PaintScene, Scene};
use linebender_resource_handle::Blob;
use peniko::kurbo::Affine;
use strake_dom::IntrinsicSizes;
use strake_dom::node::{ComputedStyles, Widget};

use super::CpuFrame;

/// A custom widget presenting the latest fallback worker frame.
///
/// Sized by the frame (replaced-element semantics); paints the frame scaled
/// to its layout box. Dropping the frame on [`Widget::disconnected`] keeps
/// unmounted fallback content at zero retained memory.
pub struct FallbackWidget {
    frame: Option<CpuFrame>,
    /// Generation presented by [`FallbackWidget::set_frame`].
    generation: u64,
    /// Generation last offered to the renderer by [`Widget::paint`].
    painted_generation: u64,
}

impl FallbackWidget {
    /// A widget with no frame (paints nothing until fed).
    pub fn new() -> Self {
        Self {
            frame: None,
            generation: 0,
            painted_generation: 0,
        }
    }

    /// Present `frame` on the next paint (requests a repaint via
    /// [`Widget::requires_redraw`] until painted).
    pub fn set_frame(&mut self, frame: CpuFrame) {
        self.generation = self.generation.wrapping_add(1);
        self.frame = Some(frame);
    }

    /// Whether a frame is currently held.
    pub fn has_frame(&self) -> bool {
        self.frame.is_some()
    }
}

impl Default for FallbackWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for FallbackWidget {
    /// A newly presented, not-yet-painted frame needs a repaint; a painted
    /// (or absent) frame is static. Without this the embedder never schedules
    /// continuous repaints and video freezes on the first composited frame.
    fn requires_redraw(&self) -> bool {
        self.frame.is_some() && self.generation != self.painted_generation
    }

    fn intrinsic_sizes(&self) -> IntrinsicSizes {
        match &self.frame {
            Some(frame) => IntrinsicSizes {
                width: Some(frame.width() as f32),
                height: Some(frame.height() as f32),
                ratio: if frame.height() == 0 {
                    None
                } else {
                    Some(frame.width() as f32 / frame.height() as f32)
                },
            },
            None => IntrinsicSizes::default(),
        }
    }

    fn paint(
        &mut self,
        _render_ctx: &mut dyn anyrender::RenderContext,
        _styles: &ComputedStyles,
        width: u32,
        height: u32,
        _scale: f64,
    ) -> Scene {
        let mut scene = Scene::new();
        let Some(frame) = &self.frame else {
            return scene;
        };
        // Share the frame's allocation with the renderer (no per-paint copy)
        // and record that this generation was offered: the redraw request
        // clears until `set_frame` presents a newer frame. A later resize
        // arrives with its own layout-driven repaint.
        let presented = (frame.width(), frame.height(), frame.shared_rgba());
        self.painted_generation = self.generation;
        let (frame_width, frame_height, pixels) = presented;
        if frame_width == 0 || frame_height == 0 || width == 0 || height == 0 {
            return scene;
        }
        let brush = peniko::ImageBrush {
            image: peniko::ImageData {
                data: Blob::new(pixels),
                format: peniko::ImageFormat::Rgba8,
                alpha_type: peniko::ImageAlphaType::Alpha,
                width: frame_width,
                height: frame_height,
            },
            sampler: peniko::ImageSampler {
                x_extend: peniko::Extend::Pad,
                y_extend: peniko::Extend::Pad,
                quality: peniko::ImageQuality::Medium,
                alpha: 1.0,
            },
        };
        scene.draw_image(
            brush.as_ref(),
            Affine::new([
                width as f64 / frame.width() as f64,
                0.0,
                0.0,
                height as f64 / frame.height() as f64,
                0.0,
                0.0,
            ]),
        );
        scene
    }

    fn disconnected(&mut self) {
        self.frame = None;
    }
}

#[test]
fn intrinsic_sizes_follow_frame() {
    let mut widget = FallbackWidget::new();
    widget.set_frame(CpuFrame::solid(64, 32, [0, 0, 0, 255]).expect("tiny frame cannot overflow"));
    let sizes = widget.intrinsic_sizes();
    assert_eq!(sizes.width, Some(64.0));
    assert_eq!(sizes.height, Some(32.0));
    assert!(widget.has_frame());
}

#[test]
fn no_frame_uses_default_sizes() {
    let widget = FallbackWidget::new();
    assert!(!widget.has_frame());
    // IntrinsicSizes has no PartialEq; compare field-wise.
    let sizes = widget.intrinsic_sizes();
    let default = IntrinsicSizes::default();
    assert_eq!(
        (sizes.width, sizes.height, sizes.ratio),
        (default.width, default.height, default.ratio)
    );
}

#[test]
fn set_frame_replaces_previous() {
    let mut widget = FallbackWidget::new();
    widget.set_frame(CpuFrame::solid(64, 32, [0, 0, 0, 255]).expect("tiny frame cannot overflow"));
    widget.set_frame(CpuFrame::solid(10, 20, [0, 0, 0, 255]).expect("tiny frame cannot overflow"));
    assert_eq!(widget.intrinsic_sizes().width, Some(10.0));
}

#[test]
fn disconnect_releases_frame_memory() {
    let mut widget = FallbackWidget::new();
    widget.set_frame(CpuFrame::solid(64, 64, [0, 0, 0, 255]).expect("tiny frame cannot overflow"));
    widget.disconnected();
    assert!(!widget.has_frame());
    assert!(
        !widget.requires_redraw(),
        "released frame must not hold the repaint loop open"
    );
}

#[test]
fn fresh_frame_requests_redraw() {
    let mut widget = FallbackWidget::new();
    assert!(
        !widget.requires_redraw(),
        "no frame held: static widget must not spin the repaint loop"
    );
    widget.set_frame(CpuFrame::solid(64, 32, [0, 0, 0, 255]).expect("tiny frame cannot overflow"));
    assert!(
        widget.requires_redraw(),
        "a newly presented frame must schedule a repaint or video freezes on the first frame"
    );
    widget.set_frame(CpuFrame::solid(64, 32, [1, 1, 1, 255]).expect("tiny frame cannot overflow"));
    assert!(
        widget.requires_redraw(),
        "every new generation re-arms the repaint request"
    );
}
