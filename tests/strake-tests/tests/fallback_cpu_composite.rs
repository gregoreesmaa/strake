//! Phase-0 CPU fallback compositing (issue #5): the supervised worker's CPU
//! frame reaches page pixels through [`FallbackWidget`](strake_fallback_supervisor::FallbackWidget).

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use std::sync::Arc;
use std::time::Duration;
use strake_dom::DocumentConfig;
use strake_fallback_supervisor::{
    CpuFrame, FallbackWidget, SpawnSpec, Supervisor, testing::FakeBackend,
};
use strake_html::{HtmlDocument, HtmlProvider};
use strake_paint::paint_scene;
use strake_traits::shell::{ColorScheme, Viewport};

const GREEN: [u8; 3] = [0, 255, 0];

#[test]
fn supervised_cpu_frame_composites_into_page_pixels() {
    // 1. Supervise a worker serving a solid-green 60x40 frame.
    let mut supervisor = Supervisor::new(FakeBackend::with_frame(CpuFrame::solid(
        60,
        40,
        [0, 255, 0, 255],
    )));
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

    // 2. Host the frame on a canvas element through the fallback widget.
    let mut doc = HtmlDocument::from_html(
        r#"<html><body style="margin:0">
            <canvas id="fb" width="60" height="40"></canvas>
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
    doc.mutate().set_custom_widget(node_id, Box::new(widget));
    doc.resolve(0.0);

    // The widget reports the frame's intrinsic size (replaced-element sizing).
    let layout = doc.get_node(node_id).unwrap().final_layout();
    assert_eq!((layout.size.width, layout.size.height), (60.0, 40.0));

    // 3. The frame's pixels land in the painted page.
    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, doc.as_mut(), 1.0, 100, 100, 0, 0),
        100,
        100,
    );
    let idx = (20 * 100 + 30) * 4;
    assert_eq!(
        [buffer[idx], buffer[idx + 1], buffer[idx + 2]],
        GREEN,
        "fallback frame must composite into the native scene"
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
