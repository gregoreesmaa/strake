use super::ElementCx;
use crate::color::{Color, ToColorColor as _};
use anyrender::PaintScene;
use kurbo::{Affine, BezPath, Cap, Circle, Join, Point, RoundedRect, Stroke, Vec2};
use peniko::Fill;
use strake_dom::local_name;
use style::dom::TElement as _;

impl ElementCx<'_, '_> {
    pub(super) fn draw_input(&self, scene: &mut impl PaintScene) {
        if self.node.local_name() != "input" {
            return;
        }

        let type_attr = self.node.attr(local_name!("type"));
        let disabled = self.node.attr(local_name!("disabled")).is_some();

        // TODO this should be coming from css accent-color, but I couldn't find how to retrieve it
        let accent_color = if disabled {
            Color::from_rgba8(209, 209, 209, 255)
        } else {
            self.style.clone_color().as_srgb_color()
        };

        match type_attr {
            Some("checkbox") => {
                let Some(checked) = self.element.checkbox_input_checked() else {
                    return;
                };
                let width = self.frame.border_box.width();
                let height = self.frame.border_box.height();
                let min_dimension = width.min(height);
                let scale = (min_dimension - 4.0).max(0.0) / 16.0;

                let frame = self.frame.border_box.to_rounded_rect(scale * 2.0);
                draw_checkbox(scene, checked, frame, self.transform, accent_color, scale);
            }
            Some("radio") => {
                let Some(checked) = self.element.checkbox_input_checked() else {
                    return;
                };
                let width = self.frame.border_box.width();
                let height = self.frame.border_box.height();
                let min_dimension = width.min(height);
                let scale = (min_dimension - 4.0).max(0.0) / 16.0;

                let frame = self.frame.border_box.to_rounded_rect(scale * 2.0);
                let center = frame.center();
                draw_radio_button(scene, checked, center, self.transform, accent_color, scale);
            }
            Some("range") => {
                let Some(data) = self.element.range_input_data() else {
                    return;
                };
                draw_range(
                    scene,
                    data,
                    self.frame.border_box,
                    self.transform,
                    accent_color,
                );
            }
            _ => {}
        }
    }
}

fn draw_checkbox(
    scene: &mut impl PaintScene,
    checked: bool,
    frame: RoundedRect,
    transform: Affine,
    accent_color: Color,
    scale: f64,
) {
    if checked {
        scene.fill(Fill::NonZero, transform, accent_color, None, &frame);
        //Tick code derived from masonry
        let mut path = BezPath::new();
        path.move_to((2.0, 9.0));
        path.line_to((6.0, 13.0));
        path.line_to((14.0, 2.0));

        path.apply_affine(Affine::translate(Vec2 { x: 2.0, y: 1.0 }).then_scale(scale));

        let style = Stroke {
            width: 2.0 * scale,
            join: Join::Round,
            miter_limit: 10.0,
            start_cap: Cap::Round,
            end_cap: Cap::Round,
            dash_pattern: Default::default(),
            dash_offset: 0.0,
        };

        scene.stroke(&style, transform, Color::WHITE, None, &path);
    } else {
        scene.fill(Fill::NonZero, transform, Color::WHITE, None, &frame);
        scene.stroke(&Stroke::default(), transform, accent_color, None, &frame);
    }
}

/// Slider track, fill, and thumb for `<input type="range">` (issue #51).
/// Horizontal only; the thumb center travels the content width linearly
/// with `value`, matching the pointer mapping in `set_range_value_from_x`.
fn draw_range(
    scene: &mut impl PaintScene,
    data: strake_dom::RangeInputData,
    frame: kurbo::Rect,
    transform: Affine,
    accent_color: Color,
) {
    const TRACK: Color = Color::from_rgba8(200, 200, 200, 255);

    let width = frame.width().max(1.0);
    let height = frame.height().max(1.0);
    let thumb_radius = (height / 2.0 - 1.0).clamp(2.0, 9.0);
    let travel = (width - 2.0 * thumb_radius).max(0.0);
    let center = Point::new(
        frame.x0 + thumb_radius + data.fraction() * travel,
        frame.y0 + height / 2.0,
    );

    let bar_height = 4.0f64.min(height / 2.0);
    let bar = kurbo::Rect::from_center_size(
        Point::new(frame.x0 + width / 2.0, frame.y0 + height / 2.0),
        (width, bar_height),
    )
    .to_rounded_rect(bar_height / 2.0);
    scene.fill(Fill::NonZero, transform, TRACK, None, &bar);

    // Filled portion from the left edge to the thumb center.
    let fill_width = (center.x - frame.x0).max(0.0);
    if fill_width > 0.0 {
        let fill = kurbo::Rect::from_origin_size(
            Point::new(frame.x0, frame.y0 + height / 2.0 - bar_height / 2.0),
            kurbo::Size::new(fill_width, bar_height),
        )
        .to_rounded_rect(bar_height / 2.0);
        scene.fill(Fill::NonZero, transform, accent_color, None, &fill);
    }

    let thumb = Circle::new(center, thumb_radius);
    scene.fill(Fill::NonZero, transform, Color::WHITE, None, &thumb);
    scene.stroke(
        &Stroke {
            width: (thumb_radius / 4.0).max(1.0),
            ..Stroke::default()
        },
        transform,
        accent_color,
        None,
        &thumb,
    );
}

fn draw_radio_button(
    scene: &mut impl PaintScene,
    checked: bool,
    center: Point,
    transform: Affine,
    accent_color: Color,
    scale: f64,
) {
    let outer_ring = Circle::new(center, 8.0 * scale);
    let gap = Circle::new(center, 6.0 * scale);
    let inner_circle = Circle::new(center, 4.0 * scale);
    if checked {
        scene.fill(Fill::NonZero, transform, accent_color, None, &outer_ring);
        scene.fill(Fill::NonZero, transform, Color::WHITE, None, &gap);
        scene.fill(Fill::NonZero, transform, accent_color, None, &inner_circle);
    } else {
        const GRAY: Color = color::palette::css::GRAY;
        scene.fill(Fill::NonZero, transform, GRAY, None, &outer_ring);
        scene.fill(Fill::NonZero, transform, Color::WHITE, None, &gap);
    }
}
