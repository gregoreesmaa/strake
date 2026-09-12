//! Phase-0 CPU fallback compositing (issue #5): the supervised worker's CPU
//! frame reaches page pixels through [`FallbackWidget`](strake_fallback_supervisor::FallbackWidget).

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use std::sync::Arc;
use std::time::Duration;
use strake_dom::DocumentConfig;
use strake_dom::Widget;
use strake_fallback_supervisor::{
    CpuFrame, FallbackWidget, SpawnSpec, Supervisor, testing::FakeBackend,
};
use strake_html::{HtmlDocument, HtmlProvider};
use strake_paint::paint_scene;
use strake_traits::shell::{ColorScheme, Viewport};

/// 60x40 frame asymmetric in both axes with a translucent region: left half
/// opaque red, right-top opaque green, right-bottom half-alpha white. A
/// vertical flip, a horizontal flip, a dropped content-box translation, or
/// ignored alpha each fail distinct assertions below (a solid frame would
/// catch none of them).
fn asymmetric_frame() -> CpuFrame {
    let mut rgba = Vec::with_capacity(4 * 60 * 40);
    for y in 0..40 {
        for x in 0..60 {
            if x < 30 {
                rgba.extend_from_slice(&[255, 0, 0, 255]);
            } else if y < 20 {
                rgba.extend_from_slice(&[0, 255, 0, 255]);
            } else {
                rgba.extend_from_slice(&[255, 255, 255, 128]);
            }
        }
    }
    CpuFrame::from_rgba(60, 40, rgba).expect("hand-built frame has exact length")
}

fn rgb(buffer: &[u8], stride: u32, x: u32, y: u32) -> [u8; 3] {
    let idx = ((y * stride + x) * 4) as usize;
    [buffer[idx], buffer[idx + 1], buffer[idx + 2]]
}

#[test]
fn supervised_cpu_frame_composites_into_page_pixels() {
    // 1. Supervise a worker serving the asymmetric 60x40 frame.
    let mut supervisor = Supervisor::new(FakeBackend::with_frame(asymmetric_frame()));
    supervisor
        .add_surface(
            SpawnSpec {
                url: String::from("https://example.com/meet"),
                width: 60,
                height: 40,
                scale: 1.0,
            },
            Duration::from_secs(0),
        )
        .expect("spawn must succeed");
    let frame = supervisor
        .pump_frame()
        .expect("active worker serves frames");

    // 2. Host the frame on a canvas OFFSET from the page origin through the
    // fallback widget (a canvas at the origin cannot catch a missing
    // content-box translation: painting at page origin still lands inside it).
    let mut doc = HtmlDocument::from_html(
        r#"<html><body style="margin:0; background:#000000;">
            <canvas id="fb" width="60" height="40" style="margin-left:30px; margin-top:20px; display:block;"></canvas>
        </body></html>"#,
        DocumentConfig {
            viewport: Some(Viewport::new(100, 100, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    let node_id = doc.query_selector("#fb").unwrap().expect("#fb");
    let mut widget = FallbackWidget::new();
    widget.set_frame(frame);
    assert!(
        widget.requires_redraw(),
        "fresh fallback frame must schedule a repaint or video freezes on the first frame"
    );
    doc.mutate().set_custom_widget(node_id, Box::new(widget));
    doc.resolve(0.0);
    assert!(
        doc.is_animating(),
        "fresh fallback frame must keep repaints scheduled until painted"
    );

    // The widget reports the frame's intrinsic size (replaced-element sizing).
    let layout = doc.get_node(node_id).unwrap().final_layout();
    assert_eq!((layout.size.width, layout.size.height), (60.0, 40.0));

    // 3. The frame's pixels land in the painted page. The canvas occupies
    // page x=30..90, y=20..60.
    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, doc.as_mut(), 1.0, 100, 100, 0, 0),
        100,
        100,
    );
    assert_eq!(
        rgb(&buffer, 100, 45, 30),
        [255, 0, 0],
        "left-half red must land at its translated page position"
    );
    assert_eq!(
        rgb(&buffer, 100, 75, 30),
        [0, 255, 0],
        "right-top green must land at its translated page position"
    );
    let [r, g, b] = rgb(&buffer, 100, 75, 50);
    assert!(
        r == g && g == b && (96..=160).contains(&r),
        "half-alpha white over black must blend to mid-gray, got [{r},{g},{b}]"
    );
    assert_eq!(
        rgb(&buffer, 100, 5, 5),
        [0, 0, 0],
        "page origin stays background: the frame must paint at the canvas box, not page origin"
    );

    // 4. Paint consumed the frame: no further repaints until a new frame arrives.
    assert!(
        !doc.is_animating(),
        "painted frame must not keep the repaint loop spinning"
    );
}

#[test]
fn widget_without_frame_paints_nothing_extra() {
    let mut doc = HtmlDocument::from_html(
        r#"<html><body style="margin:0; background:#0000ff;">
            <canvas id="fb" width="60" height="40"></canvas>
        </body></html>"#,
        DocumentConfig {
            viewport: Some(Viewport::new(100, 100, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    let node_id = doc.query_selector("#fb").unwrap().expect("#fb");
    doc.mutate()
        .set_custom_widget(node_id, Box::new(FallbackWidget::new()));
    doc.resolve(0.0);

    // Canvas with no frame paints its (transparent) default: the blue page
    // shows through where the fallback image would be.
    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, doc.as_mut(), 1.0, 100, 100, 0, 0),
        100,
        100,
    );
    let idx = (20 * 100 + 30) * 4;
    assert_eq!(
        [buffer[idx], buffer[idx + 1], buffer[idx + 2]],
        [0, 0, 255],
        "frameless widget must not paint anything"
    );
}
