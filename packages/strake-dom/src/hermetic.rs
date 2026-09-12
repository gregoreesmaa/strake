//! Hermetic font contexts for deterministic headless tests.
//!
//! The default document font context enables host system fonts, so text
//! metrics vary per OS (and measure 0x0 on minimal CI images). Tests that
//! assert on text geometry must use [`hermetic_test_font_context`], which
//! disables system-font lookup and registers only the vendored Ahem font.

use crate::AHEM_FONT;
use parley::FontContext;
use std::sync::Arc;

/// Build a `FontContext` with no host system-font lookup and only the
/// vendored Ahem test font registered.
///
/// Ahem is registered under its own name plus the three generic families so
/// unstyled UA text (`sans-serif` fallback) also shapes deterministically.
/// Mirrors the default construction in `document.rs` (`BaseDocument::new`)
/// except `system_fonts` is `false` and Ahem replaces the bullet font.
pub fn hermetic_test_font_context() -> FontContext {
    use linebender_resource_handle::Blob;
    use parley::fontique::{Collection, CollectionOptions, FontInfoOverride, SourceCache};

    let mut font_ctx = FontContext {
        source_cache: SourceCache::new_shared(),
        collection: Collection::new(CollectionOptions {
            shared: false,
            system_fonts: false,
        }),
    };
    for family in ["Ahem", "sans-serif", "serif", "monospace"] {
        font_ctx.collection.register_fonts(
            Blob::new(Arc::new(AHEM_FONT) as _),
            Some(FontInfoOverride {
                family_name: Some(family),
                ..Default::default()
            }),
        );
    }
    font_ctx
}
