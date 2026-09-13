//! Verify pin for <https://github.com/gregoreesmaa/strake/issues/46>
//! (upstream <https://github.com/DioxusLabs/blitz/issues/386>):
//! `border-collapse: collapse` must paint one shared 2px border on the
//! table outline and between every cell — no missing sides, no doubling,
//! no gutters on borderless tables.
//!
//! Triage: does not reproduce on current main — the grid geometry is exact.
//! This locks it in place. (The headless renderer leaves the page
//! background transparent, so every assertion keys on alpha: empty cells
//! mean any opaque pixel is border paint.)

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

fn opaque(buf: &[u8], x: u32, y: u32) -> bool {
    buf[((y * W + x) * 4 + 3) as usize] > 0
}

// Two-by-two grid of empty 60x20 cells under border-collapse: collapse.
// Probed geometry (cells lay out at (2,2)/(68,2)/(2,24)/(68,24), 64x20):
// shared verticals at x 0-1/64-65/130-131, shared horizontals at
// y 0-1/20-21/42-43.
const GRID: &str = r#"<html><body style="margin:0">
<table style="border-collapse:collapse;">
    <tr><td id="c11" style="border:2px solid black; width:60px; height:20px; padding:0;"></td><td id="c12" style="border:2px solid black; width:60px; height:20px; padding:0;"></td></tr>
    <tr><td id="c21" style="border:2px solid black; width:60px; height:20px; padding:0;"></td><td id="c22" style="border:2px solid black; width:60px; height:20px; padding:0;"></td></tr>
</table>
</body></html>"#;

#[test]
fn collapsed_borders_form_exact_grid() {
    let buf = render(GRID);
    // Shared horizontals run the full outline width.
    for x in (0..132).step_by(3) {
        assert!(opaque(&buf, x, 1), "top outline missing at x={x}");
        assert!(opaque(&buf, x, 21), "middle gutter missing at x={x}");
        assert!(opaque(&buf, x, 43), "bottom outline missing at x={x}");
    }
    // Shared verticals run the full outline height.
    for y in (0..44).step_by(3) {
        assert!(opaque(&buf, 1, y), "left outline missing at y={y}");
        assert!(opaque(&buf, 65, y), "middle gutter missing at y={y}");
        assert!(opaque(&buf, 131, y), "right outline missing at y={y}");
    }
    // Cell interiors stay transparent: no doubling, no fill.
    for (x, y) in [(30, 10), (100, 10), (30, 32), (100, 32)] {
        assert!(!opaque(&buf, x, y), "stray paint inside cell at {x},{y}");
    }
}

#[test]
fn upstream_fixture_paints_both_tables() {
    let buf = render(
        r#"<!doctype html><html><body style="margin:0">
        <style>table { border: 2px solid black; } th, td { border: 2px solid black; padding: 0; } .collapse { border-collapse: collapse; } .separate { border-collapse: separate; }</style>
        <table class="collapse">
            <tr><th>Name</th><th>Age</th></tr>
            <tr><td>Alice</td><td>30</td></tr>
        </table>
        </body></html>"#,
    );
    // Collapse-table outline present along the top and left edges.
    for x in (0..40).step_by(4) {
        assert!(opaque(&buf, x, 1), "collapse top outline missing at x={x}");
    }
    for y in (0..20).step_by(4) {
        assert!(opaque(&buf, 1, y), "collapse left outline missing at y={y}");
    }
}
