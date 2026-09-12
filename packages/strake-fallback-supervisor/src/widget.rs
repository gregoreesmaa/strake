//! [`FallbackWidget`]: a [`Widget`](strake_dom::Widget) compositing Phase-0
//! CPU fallback frames into the native paint scene.
//!
//! The embedder loop is: [`Supervisor`](crate::Supervisor) supervises the
//! worker → [`Supervisor::pump_frame`](crate::Supervisor::pump_frame) yields
//! the latest [`CpuFrame`](crate::CpuFrame) → [`FallbackWidget::set_frame`]
//! presents it on the fallback element's box. Phase 3 keeps this seam and
//! swaps the image upload for zero-copy GPU texture imports.

use std::sync::Arc;

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
}

impl FallbackWidget {
    /// A widget with no frame (paints nothing until fed).
    pub fn new() -> Self {
        Self { frame: None }
    }

    /// Present `frame` on the next paint.
    pub fn set_frame(&mut self, frame: CpuFrame) {
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
        if frame.width() == 0 || frame.height() == 0 || width == 0 || height == 0 {
            return scene;
        }
        let brush = peniko::ImageBrush {
            image: peniko::ImageData {
                data: Blob::new(Arc::new(frame.rgba().to_vec())),
                format: peniko::ImageFormat::Rgba8,
                alpha_type: peniko::ImageAlphaType::Alpha,
                width: frame.width(),
                height: frame.height(),
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
    widget.set_frame(CpuFrame::solid(64, 32, [0, 0, 0, 255]));
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
    widget.set_frame(CpuFrame::solid(64, 32, [0, 0, 0, 255]));
    widget.set_frame(CpuFrame::solid(10, 20, [0, 0, 0, 255]));
    assert_eq!(widget.intrinsic_sizes().width, Some(10.0));
}

#[test]
fn disconnect_releases_frame_memory() {
    let mut widget = FallbackWidget::new();
    widget.set_frame(CpuFrame::solid(64, 64, [0, 0, 0, 255]));
    widget.disconnected();
    assert!(!widget.has_frame());
}
