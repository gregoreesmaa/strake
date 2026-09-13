//! Pins for <https://github.com/gregoreesmaa/strake/issues/57>
//! (upstream <https://github.com/DioxusLabs/blitz/issues/504>):
//! collapsed tables must not paint a phantom grid for borderless tables,
//! and every edge must take its own winning side's color — not the first
//! cell's top color for the whole table.
//!
//! (The headless renderer leaves the page background transparent, so
//! borderless tests key on alpha: with empty cells, any opaque pixel is
//! border paint.)

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use std::sync::Arc;
use strake_dom::DocumentConfig;
use strake_html::{HtmlDocument, HtmlProvider};
use strake_paint::paint_scene;
use strake_traits::shell::{ColorScheme, Viewport};

const W: u32 = 300;
const H: u32 = 200;

fn render(html: &str) -> Vec<u8> {
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(W, H, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, &mut doc, 1.0, W, H, 0, 0),
        W,
        H,
    )
}

fn px(buf: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

fn opaque(buf: &[u8], x: u32, y: u32) -> bool {
    buf[((y * W + x) * 4 + 3) as usize] > 0
}

fn opaque_in(buf: &[u8], x0: u32, y0: u32, x1: u32, y1: u32) -> usize {
    let mut n = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            if opaque(buf, x, y) {
                n += 1;
            }
        }
    }
    n
}

/// Bug 1: a fully borderless collapsed table paints no grid at all.
#[test]
fn borderless_collapsed_table_paints_nothing() {
    let buf = render(
        r#"<html><body style="margin:0"><table style="border-collapse:collapse;">
            <tr><td style="width:60px; height:20px; padding:0;"></td><td style="width:60px; height:20px; padding:0;"></td></tr>
            <tr><td style="width:60px; height:20px; padding:0;"></td><td style="width:60px; height:20px; padding:0;"></td></tr>
        </table></body></html>"#,
    );
    assert_eq!(opaque_in(&buf, 0, 0, 140, 60), 0);
}

/// Bug 2: each outline edge takes its own winning side's color.
#[test]
fn outline_edges_take_their_own_side_colors() {
    let buf = render(
        r#"<html><body style="margin:0"><table style="border-collapse:collapse;">
            <tr><td style="border-top:4px solid rgb(255,0,0); border-left:4px solid rgb(0,0,255); width:60px; height:20px; padding:0;"></td>
            <td style="border-top:4px solid rgb(255,0,0); width:60px; height:20px; padding:0;"></td></tr>
        </table></body></html>"#,
    );
    assert_eq!(px(&buf, 40, 1), [255, 0, 0, 255], "top edge must be red");
    assert_eq!(px(&buf, 1, 12), [0, 0, 255, 255], "left edge must be blue");
}

/// The wider edge wins a gutter conflict, at its own thickness.
#[test]
fn wider_gutter_edge_wins_with_its_thickness() {
    let buf = render(
        r#"<html><body style="margin:0"><table style="border-collapse:collapse;">
            <tr><td style="border-bottom:6px solid rgb(0,128,0); width:60px; height:20px; padding:0;"></td></tr>
            <tr><td style="border-top:2px solid rgb(255,0,0); width:60px; height:20px; padding:0;"></td></tr>
        </table></body></html>"#,
    );
    // Gutter between the rows: 6px tall (y 20..26), green.
    for y in 20..26 {
        assert_eq!(
            px(&buf, 30, y),
            [0, 128, 0, 255],
            "gutter must be green at y={y}"
        );
    }
    assert!(!opaque(&buf, 30, 19), "gutter must start at y=20");
    assert!(!opaque(&buf, 30, 26), "gutter must end before y=26");
}

/// `hidden` suppresses the conflicting solid edge.
#[test]
fn hidden_suppresses_conflicting_edge() {
    let buf = render(
        r#"<html><body style="margin:0"><table style="border-collapse:collapse;">
            <tr><td style="border-bottom:2px hidden black; width:60px; height:20px; padding:0;"></td></tr>
            <tr><td style="border-top:2px solid rgb(255,0,0); width:60px; height:20px; padding:0;"></td></tr>
        </table></body></html>"#,
    );
    assert_eq!(opaque_in(&buf, 0, 18, 70, 28), 0);
}

/// A bordered table with borderless cells still paints its own outline.
#[test]
fn table_own_border_outlines_borderless_cells() {
    let buf = render(
        r#"<html><body style="margin:0"><table style="border-collapse:collapse; border:3px solid rgb(255,0,0);">
            <tr><td style="width:60px; height:20px; padding:0;"></td><td style="width:60px; height:20px; padding:0;"></td></tr>
        </table></body></html>"#,
    );
    assert_eq!(px(&buf, 40, 1), [255, 0, 0, 255], "top outline must be red");
    assert_eq!(
        px(&buf, 1, 12),
        [255, 0, 0, 255],
        "left outline must be red"
    );
}

/// Uniform borders reproduce the legacy single-color grid exactly.
#[test]
fn uniform_borders_keep_legacy_grid() {
    let buf = render(
        r#"<html><body style="margin:0"><table style="border-collapse:collapse;">
            <tr><td style="border:2px solid black; width:60px; height:20px; padding:0;"></td><td style="border:2px solid black; width:60px; height:20px; padding:0;"></td></tr>
            <tr><td style="border:2px solid black; width:60px; height:20px; padding:0;"></td><td style="border:2px solid black; width:60px; height:20px; padding:0;"></td></tr>
        </table></body></html>"#,
    );
    for x in (0..132).step_by(3) {
        assert!(opaque(&buf, x, 1), "top outline missing at x={x}");
        assert!(opaque(&buf, x, 21), "middle gutter missing at x={x}");
        assert!(opaque(&buf, x, 43), "bottom outline missing at x={x}");
    }
    for y in (0..44).step_by(3) {
        assert!(opaque(&buf, 1, y), "left outline missing at y={y}");
        assert!(opaque(&buf, 65, y), "middle gutter missing at y={y}");
        assert!(opaque(&buf, 131, y), "right outline missing at y={y}");
    }
    for (x, y) in [(30, 10), (100, 10), (30, 32), (100, 32)] {
        assert!(!opaque(&buf, x, y), "stray paint inside cell at {x},{y}");
    }
}
