//! Hermeticity pins (issue #2): text geometry must be identical on every OS
//! and must never measure 0x0 (the vacuous-pass failure mode of
//! `tests/strake-tests/tests/br_trailing_line.rs` on fontless CI images).

use strake_test_harness::Harness;

#[test]
fn ahem_text_measures_nonzero_and_deterministic() {
    for _ in 0..2 {
        let harness = Harness::from_html(
            r#"<html><body style="margin:0; font-family:Ahem; font-size:16px; line-height:16px;">
                <div id="t" style="display:inline-block;">XXXX</div>
            </body></html>"#,
        );
        let rect = harness.layout_rect("#t");
        // Four Ahem glyphs at 16px: advance is exactly 1em per glyph.
        // Assert nonzero FIRST so a fontless environment fails loudly
        // instead of passing vacuously.
        assert!(rect.width > 0.0, "text measured 0x0: no usable font");
        assert_eq!(rect.width, 64.0);
        assert_eq!(rect.height, 16.0);
    }
}

#[test]
fn generic_families_fall_back_to_embedded_font() {
    for family in ["sans-serif", "serif", "monospace"] {
        let harness = Harness::from_html(&format!(
            r#"<html><body style="margin:0; font-family:{family}; font-size:20px;">
                <div id="t" style="display:inline-block;">XX</div>
            </body></html>"#
        ));
        let rect = harness.layout_rect("#t");
        assert!(rect.width > 0.0, "family {family} measured 0x0");
        assert_eq!(rect.width, 40.0);
    }
}
