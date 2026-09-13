# Hermetic Headless Testing Suite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make headless layout tests hermetic and deterministic across Linux/macOS/Windows, pin the event-dispatch order contract, and prove paint-only restyles skip Taffy layout.

**Architecture:** Vendor the public-domain Ahem test font into `strake-dom` (mirroring the existing `BULLET_FONT` pattern), expose a `hermetic_test_font_context()` constructor that disables system fonts, default `HarnessOptions` to it, then add focused spec-pinning suites (DOM recycling, flex/grid geometry, capture-phase events, invalidation isolation) as new files under `tests/strake-tests/tests/`.

**Tech Stack:** Rust 2024, parley 0.11.1 / fontique 0.11.1 (`CollectionOptions { shared, system_fonts }`, `register_fonts` with `FontInfoOverride`), Stylo `RestyleDamage`, Taffy layout, Boa-based `ScriptDocument` for event tests.

**Spec:** GitHub issue #2 — `[Epic] Comprehensive Headless & Cross-Platform Testing Suite` (`gh issue view 2 --repo gregoreesmaa/strake`). The plan argues from that issue; executors read both.

## Global Constraints

- Rust 2024 Edition, MSRV 1.91.0+.
- NEVER add new external crates to any `Cargo.toml` without explicit user permission (AGENTS.md). All work below uses existing workspace dependencies only.
- Library crates (`packages/*`) MUST use typed errors (`thiserror`); never `anyhow`/`Box<dyn Error>` in public APIs. No new `unsafe` blocks; every `unsafe` needs a `// SAFETY:` comment (none expected here).
- Never store raw `&Node` references across event boundaries; use `NodeId`.
- Be surgical and minimal: touch only the files listed per task. Never reformat unedited files.
- Verify with `cargo check --workspace`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --all -- --check`, and the test commands named in each task.

## File Structure

- Modify: `packages/strake-dom/src/lib.rs` — add `AHEM_FONT` const next to `BULLET_FONT` (lines 32-33).
- Create: `packages/strake-dom/assets/Ahem.ttf` — vendored public-domain test font (provenance noted in `NOTICE`).
- Create: `packages/strake-dom/src/hermetic.rs` — `pub fn hermetic_test_font_context() -> FontContext` (system fonts off, Ahem registered under `Ahem`, `sans-serif`, `serif`, `monospace`).
- Modify: `packages/strake-dom/src/document.rs` — add `layout_passes: u64` counter field, increment in `resolve_layout`, add `pub fn layout_pass_count()` accessor.
- Modify: `packages/strake-test-harness/src/harness.rs` — add `HarnessOptions.font_ctx: Option<FontContext>`, default to hermetic, pass through in `into_config`.
- Create: `tests/strake-tests/tests/hermetic_fonts.rs` — nonzero-width + cross-test determinism pins.
- Create: `tests/strake-tests/tests/dom_slotmap_recycling.rs` — NodeId ABA + subtree detach pins.
- Create: `tests/strake-tests/tests/layout_geometry.rs` — flex auto-margins/wrap + grid `fr`/gap pins via `Harness`.
- Modify: `packages/strake-vibey-script/src/runtime.rs` — `dispatch_event_inner` gains capture-phase traversal honoring the stored `capture` flag.
- Create: `packages/strake-vibey-script/tests/capture_phase.rs` — capture/bubble order + `stopPropagation` pins (mirrors `tests/dom.rs` patterns).
- Create: `tests/strake-tests/tests/restyle_layout_bypass.rs` — paint-only mutation triggers zero Taffy passes.
- Modify: `NOTICE` — Ahem provenance note.

---

### Task 1: Restore fmt-clean baseline (CI-green prerequisite)

`cargo fmt --all -- --check` currently fails (verified: import-ordering diffs in `apps/browser/src/*.rs`, `examples/wgpu_texture/src/demo_renderer.rs`). The `Rustfmt` CI job is red until this is fixed. No code changes beyond formatting.

**Files:**
- Modify: whatever `cargo fmt --all` rewrites (formatting only, no hand edits).

**Interfaces:**
- Consumes: nothing. Produces: fmt-clean tree for all later tasks.

- [ ] **Step 1: Run the formatter**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo fmt --all
```

- [ ] **Step 2: Confirm only formatting changed and checks pass**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && git status --short && cargo fmt --all -- --check && echo FMT_OK
```

Expected: `FMT_OK`, and `git diff --stat` shows only moved import lines (no logic changes).

- [ ] **Step 3: Run fast verification gates**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo check --workspace 2>&1 | tail -n 3 && cargo clippy --workspace -- -D warnings 2>&1 | tail -n 3
```

Expected: both finish with no errors/warnings.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "style: apply cargo fmt import ordering (fix Rustfmt CI job)"
```

### Task 2: Vendor Ahem.ttf and expose the hermetic font context

The default `BaseDocument` font context enables host system fonts (`packages/strake-dom/src/document.rs:374-388`) and only embeds `moz-bullet-font.otf` (`src/lib.rs:32-33`), so text metrics vary per OS and measure 0x0 on minimal CI images. This task vendors the Ahem test font and adds a constructor that builds a `FontContext` with `system_fonts: false`, mirroring the exact construction pattern at `document.rs:374-388`.

**Files:**
- Create: `packages/strake-dom/assets/Ahem.ttf`
- Modify: `packages/strake-dom/src/lib.rs:32-33`
- Create: `packages/strake-dom/src/hermetic.rs`
- Modify: `NOTICE`

**Interfaces:**
- Consumes: `parley::fontique::{Collection, CollectionOptions, SourceCache, FontInfoOverride}`, `linebender_resource_handle::Blob` (mirror the `use` statements already present at the top of `packages/strake-dom/src/document.rs` — copy them verbatim).
- Produces: `pub const AHEM_FONT: &[u8]` and `pub fn hermetic_test_font_context() -> FontContext` for use by Task 3 and Task 5.

- [ ] **Step 1: Obtain Ahem.ttf and verify its license**

Fetch the font from the canonical public-domain source and confirm it is distributable:

```bash
curl -sSL -o packages/strake-dom/assets/Ahem.ttf https://github.com/w3c/csswg-test/raw/master/fonts/Ahem.ttf && ls -l packages/strake-dom/assets/Ahem.ttf && file packages/strake-dom/assets/Ahem.ttf
```

Expected: file exists, `file` reports TrueType font data. If the fetch fails (offline), check for a local copy under `wpt/tests` or `~/.fonts`; if none exists, STOP and report — do not substitute another font silently, because every glyph metric below is Ahem-specific (ascent 0.8em, descent 0.2em, advance 1.0em per issue #2).

- [ ] **Step 2: Record provenance in NOTICE**

Append a short entry to `NOTICE` (read the file first and match its existing format):

```text
Ahem test font (packages/strake-dom/assets/Ahem.ttf): public domain,
originally by Todd Fahrner, distributed via w3c/csswg-test for CSS
conformance testing. Test-only asset; never selected outside tests.
```

- [ ] **Step 3: Expose the font bytes in lib.rs**

In `packages/strake-dom/src/lib.rs`, directly below line 33 (`pub const BULLET_FONT ...`), add:

```rust
/// Ahem test font for hermetic layout tests: every glyph advances exactly
/// 1em with ascent 0.8em / descent 0.2em, independent of host OS fonts.
/// Test-only; see `hermetic_test_font_context`.
pub const AHEM_FONT: &[u8] = include_bytes!("../assets/Ahem.ttf");
```

- [ ] **Step 4: Implement the hermetic constructor**

Create `packages/strake-dom/src/hermetic.rs`:

```rust
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
    use parley::fontique::{Collection, CollectionOptions, FontInfoOverride, SourceCache};
    // Blob import: copy the exact `Blob` path used by document.rs
    // (grep `use .*Blob` in packages/strake-dom/src/document.rs and reuse it).
    use linebender_resource_handle::Blob;

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
```

If the `Blob` path differs in `document.rs`, use the actual one — the grep in the comment is mandatory, not optional. Register `mod hermetic;` and re-export in `lib.rs` following the existing `mod`/`pub use` style in that file:

```rust
pub use hermetic::hermetic_test_font_context;
```

- [ ] **Step 5: Verify compilation and font registration**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo check -p strake-dom 2>&1 | tail -n 3
```

Expected: success. Then write a throwaway probe under `/tmp` (NOT in the repo) that builds the context and asserts the collection is non-empty, run it, and keep it for the reviewer:

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p strake-dom hermetic 2>&1 | tail -n 5
```

(Expected: 0 tests match — the real tests land in Task 5. This step only proves the crate builds.)

- [ ] **Step 6: Commit**

```bash
git add packages/strake-dom/assets/Ahem.ttf packages/strake-dom/src/hermetic.rs packages/strake-dom/src/lib.rs NOTICE && git commit -m "feat: vendor Ahem font and add hermetic_test_font_context (issue #2)"
```

### Task 3: Default HarnessOptions to the hermetic font context

`HarnessOptions::into_config` (`packages/strake-test-harness/src/harness.rs:36-51`) falls through to `..Default::default()`, leaving `font_ctx: None`, which selects the system-font context. This wires the hermetic context through as the default while keeping an explicit opt-out. No new dependencies: `FontContext` is already re-exported by `strake-dom` (see `use strake_dom::{DocumentConfig, FontContext}` in `tests/strake-tests/tests/br_trailing_line.rs`).

**Files:**
- Modify: `packages/strake-test-harness/src/harness.rs:1-51`

**Interfaces:**
- Consumes: `strake_dom::hermetic_test_font_context` (Task 2).
- Produces: `HarnessOptions { font_ctx: Option<FontContext> }` defaulting to hermetic; `into_config` forwards it.

- [ ] **Step 1: Add the field and default**

```rust
use strake_dom::{DocGuard, DocGuardMut, Document, DocumentConfig, FontContext, hermetic_test_font_context};

/// Options controlling document construction for a [`Harness`].
pub struct HarnessOptions {
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub color_scheme: ColorScheme,
    /// Base url which relative URLs are resolved against
    pub base_url: Option<String>,
    /// Net provider used to fetch sub-resources (stylesheets, images, fonts, etc)
    pub net_provider: Option<Arc<dyn NetProvider>>,
    /// Parley font context. Defaults to the hermetic Ahem-only context
    /// (no host system fonts) so text metrics are identical on every OS.
    /// Set to `None` to use the engine default (host system fonts).
    pub font_ctx: Option<FontContext>,
}
```

```rust
impl Default for HarnessOptions {
    fn default() -> Self {
        Self {
            width: 800,
            height: 600,
            scale: 1.0,
            color_scheme: ColorScheme::Light,
            base_url: None,
            net_provider: None,
            font_ctx: Some(hermetic_test_font_context()),
        }
    }
}
```

```rust
impl HarnessOptions {
    fn into_config(self) -> DocumentConfig {
        DocumentConfig {
            viewport: Some(Viewport::new(
                self.width,
                self.height,
                self.scale,
                self.color_scheme,
            )),
            base_url: self.base_url,
            net_provider: self.net_provider,
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            font_ctx: self.font_ctx,
            ..Default::default()
        }
    }
}
```

- [ ] **Step 2: Run the harness-backed tests and triage**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p strake-tests --test harness_smoke 2>&1 | tail -n 8
```

Expected: PASS (that suite asserts fixed-size boxes, no text metrics). Then run the full integration suite:

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p strake-tests 2>&1 | grep -E "test result: FAILED|failures:" | head -n 20
```

For each failure, determine: (a) the test was asserting system-font-specific pixel widths — update the test to Ahem-derived values in its own follow-up edit, or (b) a real regression — STOP and report. Do not weaken assertions to make them pass; Ahem advances are exactly 1em, so recompute expected widths as `n_chars * font_size_px`.

- [ ] **Step 3: Commit**

```bash
git add packages/strake-test-harness/src/harness.rs && git commit -m "feat: default HarnessOptions to hermetic Ahem font context (issue #2)"
```

### Task 4: Add layout-pass counter to BaseDocument (invalidation observability)

`compute_layout_damage` is `pub(crate)` (`packages/strake-dom/src/layout/damage.rs:199`), so integration tests cannot observe whether a paint-only mutation skipped Taffy work. This adds a minimal public counter: increment it in `resolve_layout` (`packages/strake-dom/src/resolve.rs:381-393`, next to the `taffy::compute_root_layout` call) and expose a getter. Read the `BaseDocument` struct definition in `packages/strake-dom/src/document.rs` (around line 245, near the `font_ctx` field) and add the field following existing style.

**Files:**
- Modify: `packages/strake-dom/src/document.rs` (struct field + accessor)
- Modify: `packages/strake-dom/src/resolve.rs` (`resolve_layout` increment)

**Interfaces:**
- Consumes: nothing new. Produces: `pub fn layout_pass_count(&self) -> u64` for Task 7.

- [ ] **Step 1: Add the field and accessor**

```rust
// In the BaseDocument struct, next to the other resolve-phase state:
/// Number of times `resolve_layout` ran Taffy layout since construction.
/// Test observability for restyle-isolation pins (issue #2): paint-only
/// mutations must not increment this counter.
layout_passes: u64,
```

Initialize to `0` at every `BaseDocument` construction site in `document.rs` (find them via the struct literal; there is at least the one in `BaseDocument::new`).

```rust
/// Number of Taffy layout passes executed by `resolve_layout` since the
/// document was constructed.
pub fn layout_pass_count(&self) -> u64 {
    self.layout_passes
}
```

- [ ] **Step 2: Increment in resolve_layout**

In `resolve_layout` (`resolve.rs:381-393`), increment immediately before/after the `taffy::compute_root_layout(...)` call:

```rust
self.layout_passes += 1;
```

If `resolve_layout` early-returns on any path, the increment must sit after all early-returns so the counter means "Taffy actually ran".

- [ ] **Step 3: Verify**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo check -p strake-dom 2>&1 | tail -n 3 && cargo test -p strake-tests --test style_property_invalidation 2>&1 | tail -n 5
```

Expected: check passes; existing invalidation tests still pass.

- [ ] **Step 4: Commit**

```bash
git add packages/strake-dom/src/document.rs packages/strake-dom/src/resolve.rs && git commit -m "feat: add BaseDocument::layout_pass_count for restyle-isolation tests (issue #2)"
```

### Task 5: Hermetic font pins + DOM recycling + attribute reflection suites

New integration tests that fail on any host-font dependence or NodeId aliasing. Patterns to copy: `Harness::from_html` + `layout_rect` from `tests/strake-tests/tests/harness_smoke.rs:10-35; `mutator.create_element/create_text_node/append_children` + `qname` helper from `tests/strake-tests/tests/detached_attribute.rs:1-40; `color_of` helper from `tests/strake-tests/tests/style_property_invalidation.rs:32-54` (copy it verbatim into the new test file where needed).

**Files:**
- Create: `tests/strake-tests/tests/hermetic_fonts.rs`
- Create: `tests/strake-tests/tests/dom_slotmap_recycling.rs`
- Test: existing suite must stay green (no migration of the other 45 files in this plan — that is follow-up work, see Task 9).

**Interfaces:**
- Consumes: `Harness` (Task 3 default), `BaseDocument::get_node` (`document.rs:591`), `Mutator::{create_element, create_text_node, append_children, remove_and_drop_node, set_attribute, clear_attribute}` (`mutator.rs:140,144,268,396,554,641`).
- Produces: three passing test files proving hermeticity and DOM invariants.

- [ ] **Step 1: Write the failing hermetic pin**

Create `tests/strake-tests/tests/hermetic_fonts.rs`:

```rust
//! Hermeticity pins (issue #2): text geometry must be identical on every OS
//! and must never measure 0x0 (the vacuous-pass failure mode of
//! `tests/strake-tests/tests/br_trailing_line.rs` on fontless CI images).

use strake_test_harness::Harness;

#[test]
fn ahem_text_measures_nonzero_and_deterministic() {
    for _ in 0..2 {
        let harness = Harness::from_html(
            r#"<html><body style="margin:0; font-family:Ahem; font-size:16px; line-height:16px;">
                <div id="t">XXXX</div>
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
                <div id="t">XX</div>
            </body></html>"#
        ));
        let rect = harness.layout_rect("#t");
        assert!(rect.width > 0.0, "family {family} measured 0x0");
        assert_eq!(rect.width, 40.0);
    }
}
```

Run: `cargo test -p strake-tests --test hermetic_fonts`. Expected before Task 2/3: FAIL (0x0 or host-dependent widths). After: PASS. If `rect.width` is close to but not exactly 64.0 (e.g. sub-pixel shaping), print the actual value, confirm it is identical across two runs in this container, and pin THAT value with a comment explaining why — but keep the `> 0.0` assertion regardless.

- [ ] **Step 2: Write the DOM recycling + attribute tests**

Create `tests/strake-tests/tests/dom_slotmap_recycling.rs`:

```rust
//! SlotMap generational-stability pins (issue #2, section 3A).

use std::sync::Arc;
use strake_dom::{DocumentConfig, LocalName, QualName, ns};
use strake_html::{HtmlDocument, HtmlProvider};
use strake_traits::shell::{ColorScheme, Viewport};

fn qname(local: &str) -> QualName {
    QualName {
        prefix: None,
        ns: ns!(html),
        local: LocalName::from(local),
    }
}

fn make_doc() -> HtmlDocument {
    let doc = HtmlDocument::from_html(
        r#"<!DOCTYPE html><html><head><style>
            body { margin: 0; }
            #btn[disabled] { color: rgb(255, 0, 0); }
            #btn { color: rgb(0, 0, 0); }
        </style></head><body><div id="root"></div></body></html>"#,
        DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            font_ctx: Some(strake_dom::hermetic_test_font_context()),
            ..Default::default()
        },
    );
    let mut doc = doc;
    doc.resolve(0.0);
    doc
}

fn color_of(doc: &strake_dom::BaseDocument, selector: &str) -> [u8; 3] {
    // Verbatim from tests/strake-tests/tests/style_property_invalidation.rs:45-56.
    let node_id = doc.query_selector(selector).unwrap().unwrap();
    let node = doc.get_node(node_id).unwrap();
    let styles = node.primary_styles().unwrap();
    let color = styles.clone_color().into_srgb_legacy();
    let srgb = color.raw_components();
    [
        (srgb[0] * 255.0).round() as u8,
        (srgb[1] * 255.0).round() as u8,
        (srgb[2] * 255.0).round() as u8,
    ]
}

#[test]
fn stale_node_id_never_aliases_recycled_slot() {
    let mut doc = make_doc();
    let root = doc.query_selector("#root").unwrap().unwrap();
    let mut stale = None;
    for i in 0..10_000 {
        let mut m = doc.mutate();
        let el = m.create_element(qname("div"), Vec::new());
        m.append_children(root, &[el]);
        if i == 0 {
            stale = Some(el);
        }
        m.remove_and_drop_node(el);
    }
    drop(doc.mutate());
    doc.resolve(0.0);
    // The very first id must be dead even after 10k slot reuses.
    assert!(doc.get_node(stale.unwrap()).is_none());
    // And fresh allocations still work.
    let mut m = doc.mutate();
    let el = m.create_element(qname("div"), Vec::new());
    m.append_children(root, &[el]);
    drop(m);
    doc.resolve(0.0);
    assert!(doc.get_node(el).is_some());
}

#[test]
fn detached_subtree_reparent_keeps_document_order() {
    let mut doc = make_doc();
    let root = doc.query_selector("#root").unwrap().unwrap();
    let mut m = doc.mutate();
    let parent = m.create_element(qname("div"), Vec::new());
    let child = m.create_element(qname("span"), Vec::new());
    let text = m.create_text_node("hi");
    m.append_children(child, &[text]);
    m.append_children(parent, &[child]);
    m.append_children(root, &[parent]);
    // Detach the whole subtree, then re-attach: descendants survive.
    m.remove_node(parent);
    assert!(doc.get_node(child).is_some());
    m.append_children(root, &[parent]);
    drop(m);
    doc.resolve(0.0);
    assert!(doc.query_selector("span").unwrap().is_some());
    assert_eq!(doc.query_selector("#root div span").unwrap().is_some(), true);
}

#[test]
fn disabled_attribute_reflection_changes_matched_style() {
    let mut doc = make_doc();
    let root = doc.query_selector("#root").unwrap().unwrap();
    let mut m = doc.mutate();
    let btn = m.create_element(qname("button"), Vec::new());
    m.set_attribute(btn, qname("id"), "btn");
    m.append_children(root, &[btn]);
    drop(m);
    doc.resolve(0.0);
    assert_eq!(color_of(&doc, "#btn"), [0, 0, 0]);
    let mut m = doc.mutate();
    m.set_attribute(btn, qname("disabled"), "");
    drop(m);
    doc.resolve(0.0);
    assert_eq!(color_of(&doc, "#btn"), [255, 0, 0]);
    let mut m = doc.mutate();
    m.clear_attribute(btn, qname("disabled"));
    drop(m);
    doc.resolve(0.0);
    assert_eq!(color_of(&doc, "#btn"), [0, 0, 0]);
}
```

API notes (all verified by grep): `doc.mutate()` returns the mutator used in `detached_attribute.rs`; `remove_and_drop_node` at `mutator.rs:554`; `remove_node` at `mutator.rs:533` (detaches, keeps node alive); `get_node` at `document.rs:591`. If `remove_node` on a parent drops descendants instead of detaching, adjust the reparent test to the actual semantics — but keep an assertion that documents them.

- [ ] **Step 3: Run both suites**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p strake-tests --test hermetic_fonts --test dom_slotmap_recycling 2>&1 | grep -E "^test |test result" | head -n 20
```

Expected: all PASS.

- [ ] **Step 4: Commit**

```bash
git add tests/strake-tests/tests/hermetic_fonts.rs tests/strake-tests/tests/dom_slotmap_recycling.rs && git commit -m "test: add hermetic font pins and SlotMap recycling suite (issue #2)"
```

### Task 6: CSS layout geometry suite (flex + grid)

Pins from issue #2 section 3B, restricted to properties Taffy demonstrably supports today (flex auto-margins, wrapping, `fr` tracks, gaps). Uses only `Harness::from_html` + `layout_rect` (pattern from `harness_smoke.rs:10-35`). No text measurement — pure box geometry, so these pass identically with or without system fonts.

**Files:**
- Create: `tests/strake-tests/tests/layout_geometry.rs`

**Interfaces:**
- Consumes: `Harness` (Task 3). Produces: passing geometry pins.

- [ ] **Step 1: Write the failing test**

```rust
//! Flexbox/Grid geometry pins (issue #2, section 3B). Pure box geometry;
//! no text measurement, so results are font-independent.

use strake_test_harness::Harness;

#[test]
fn flex_auto_margins_absorb_free_space() {
    let harness = Harness::from_html(
        r#"<html><body style="margin:0">
            <div id="row" style="display:flex; width:300px; height:50px;">
                <div id="a" style="width:50px; height:50px; margin-left:auto;"></div>
            </div>
        </body></html>"#,
    );
    let row = harness.layout_rect("#row");
    let a = harness.layout_rect("#a");
    assert_eq!((row.x, row.y, row.width), (0.0, 0.0, 300.0));
    assert_eq!((a.x, a.width), (250.0, 50.0));
}

#[test]
fn flex_wrap_creates_two_rows() {
    let harness = Harness::from_html(
        r#"<html><body style="margin:0">
            <div id="row" style="display:flex; flex-wrap:wrap; width:120px;">
                <div id="a" style="width:100px; height:10px;"></div>
                <div id="b" style="width:100px; height:10px;"></div>
            </div>
        </body></html>"#,
    );
    let a = harness.layout_rect("#a");
    let b = harness.layout_rect("#b");
    assert_eq!((a.x, a.y), (0.0, 0.0));
    assert_eq!((b.x, b.y), (0.0, 10.0));
}

#[test]
fn grid_fr_tracks_split_free_space_with_gap() {
    let harness = Harness::from_html(
        r#"<html><body style="margin:0">
            <div id="g" style="display:grid; grid-template-columns:1fr 2fr; column-gap:10px; width:310px; height:40px;">
                <div id="a"></div><div id="b"></div>
            </div>
        </body></html>"#,
    );
    // Free space 310-10=300 split 1:2 -> 100 / 200.
    let a = harness.layout_rect("#a");
    let b = harness.layout_rect("#b");
    assert_eq!((a.x, a.width), (0.0, 100.0));
    assert_eq!((b.x, b.width), (110.0, 200.0));
}
```

- [ ] **Step 2: Run and reconcile with Taffy's actual behavior**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p strake-tests --test layout_geometry -- --nocapture 2>&1 | tail -n 15
```

If any assertion fails, print actual rects, then decide: (a) expectation was wrong per CSS spec (flex/grid) — fix the TEST and cite the spec rule in a comment; (b) engine bug — STOP and file it, do not encode buggy values. Subgrid and dense auto-placement are explicitly OUT of scope (Taffy does not implement subgrid); note that in the file header.

- [ ] **Step 3: Commit**

```bash
git add tests/strake-tests/tests/layout_geometry.rs && git commit -m "test: add flex/grid geometry pins (issue #2)"
```

### Task 7: Implement capture-phase dispatch in `dispatch_event_inner`

Verified gap: listeners store a `capture: bool` flag (`packages/strake-vibey-script/src/state.rs:49`, parsed in `src/dom/node.rs:595-643`), but `dispatch_event_inner` (`packages/strake-vibey-script/src/runtime.rs:909-~1010`) iterates `chain` strictly target-to-root (bubble order) and never reads the flag — capture listeners fire in the wrong order. `chain[0]` is the target, ascending to the root (`node_chain`, `packages/strake-dom/src/traversal.rs:129-136`). `stopPropagation` (`stopped`) and `stopImmediatePropagation` (`stopped_immediate`) handling already exists (`runtime.rs:985-1000`); the existing bubble pin is `click_events_bubble_and_stop_propagation` (`tests/dom.rs:206-239`).

**Files:**
- Modify: `packages/strake-vibey-script/src/runtime.rs` (`dispatch_event_inner` only)
- Create: `packages/strake-vibey-script/tests/capture_phase.rs`

**Interfaces:**
- Consumes: listener `capture` flag, `EventState::stopped/stopped_immediate` probes (`event_ref` closure pattern at `runtime.rs:947-952`).
- Produces: W3C two-phase order (capture root-to-target, then bubble target-to-root). `DioxusEventHandler` (`dioxus_document.rs:267`) is untouched and stays bubble-only (noted as follow-up).

- [ ] **Step 1: Read the full function before touching it**

Read `runtime.rs:909-1010` end to end, including the window-listener tail after the `'chain` loop. Note exactly: where `callbacks` are gathered, where `once` listeners are retained, where `on<event>` property handlers are read, where `currentTarget` is defined, and where window listeners run.

- [ ] **Step 2: Split traversal into capture + bubble phases**

Restructure the `'chain` loop into two loops over the same per-node body, keeping every behavior inside the body identical (once-removal, `on<event>` handling, `currentTarget`, error reporting):

```rust
// Phase 1 (capture): root -> target, capture listeners only. Skipped
// for non-bubbling events (matches current target-only behavior).
if bubbles {
    for &node_id in chain.iter().rev() {
        // ... same per-node body, but gather ONLY listeners with
        // `l.capture == true`; skip `on<event>` property handlers ...
        // `stopped` (stopPropagation) breaks the outer loop but finishes
        // the current node's callbacks; `stopped_immediate` breaks 'chain.
    }
}
// Phase 2 (bubble): target -> root, bubble listeners only.
'chain: for &node_id in chain {
    // ... same per-node body, but gather ONLY `l.capture == false`
    // plus the `on<event>` property handler ...
}
```

Rules: (a) `on<event>` property handlers are bubble-phase (target + bubble), never capture. (b) A `stopped` flag set during capture must prevent the bubble phase entirely. (c) Window listeners after both phases run only if propagation was not stopped (preserve current tail behavior — read it first; if the current tail runs unconditionally, keep it unconditional and note why). (d) The `may_have_listeners` fast path (lines 921-943) stays as-is.

- [ ] **Step 3: Write the capture test file**

Create `packages/strake-vibey-script/tests/capture_phase.rs`, mirroring the `doc_from_html` + `text_of_selector` helpers and the `DomEvent::new(id, synthetic_click_event)` dispatch pattern from `packages/strake-vibey-script/tests/dom.rs:1-14,160-202` (copy the helper bodies and imports, including `use keyboard_types::Modifiers`):

```rust
//! Capture-phase event dispatch pins (issue #2, section 3C).

use strake_dom::{Document, DocumentConfig};
use strake_traits::events::DomEvent;
use strake_vibey_script::ScriptDocument;
use keyboard_types::Modifiers;

fn doc_from_html(html: &str) -> ScriptDocument {
    let mut doc = ScriptDocument::from_html(html, DocumentConfig::default());
    doc.execute_scripts();
    doc
}

fn text_of_selector(doc: &ScriptDocument, selector: &str) -> String {
    // Verbatim from packages/strake-vibey-script/tests/dom.rs:14-21.
    let inner = doc.inner();
    let node_id = inner
        .query_selector(selector)
        .unwrap()
        .unwrap_or_else(|| panic!("no node matching {selector}"));
    inner.get_node(node_id).unwrap().text_content()
}
```

Then the tests:

```rust
fn click_on(doc: &ScriptDocument, selector: &str) -> DomEvent {
    let inner = doc.inner();
    let id = inner.query_selector(selector).unwrap().unwrap();
    DomEvent::new(
        id,
        inner.get_node(id).unwrap().synthetic_click_event(Modifiers::empty()),
    )
}

#[test]
fn capture_listeners_fire_root_to_target_before_bubble() {
    let mut doc = doc_from_html(
        r#"
        <html><body>
            <div id="outer"><div id="middle"><button id="inner">hi</button></div></div>
            <div id="out"></div>
            <script>
                const log = [];
                const record = (name) => () => {
                    log.push(name);
                    document.getElementById("out").textContent = log.join(",");
                };
                for (const id of ["outer", "middle", "inner"]) {
                    document.getElementById(id).addEventListener("click", record("cap:" + id), true);
                    document.getElementById(id).addEventListener("click", record("bub:" + id), false);
                }
            </script>
        </body></html>
        "#,
    );
    doc.dispatch_dom_event(click_on(&doc, "#inner"));
    assert_eq!(
        text_of_selector(&doc, "#out"),
        "cap:outer,cap:middle,cap:inner,bub:inner,bub:middle,bub:outer"
    );
}

#[test]
fn stop_propagation_in_capture_prevents_bubble_phase() {
    let mut doc = doc_from_html(
        r#"
        <html><body>
            <div id="outer"><button id="inner">hi</button></div>
            <div id="out"></div>
            <script>
                const log = [];
                const record = (name) => () => {
                    log.push(name);
                    document.getElementById("out").textContent = log.join(",");
                };
                document.getElementById("outer").addEventListener("click", (e) => {
                    record("cap:outer")();
                    e.stopPropagation();
                }, true);
                document.getElementById("inner").addEventListener("click", record("bub:inner"), false);
                document.getElementById("outer").addEventListener("click", record("bub:outer"), false);
            </script>
        </body></html>
        "#,
    );
    doc.dispatch_dom_event(click_on(&doc, "#inner"));
    assert_eq!(text_of_selector(&doc, "#out"), "cap:outer");
}
```

Assumption to verify while implementing: the JS `Event` wrapper exposes `stopPropagation()` (the existing test at `dom.rs:216-219` calls `event.stopPropagation()`, so yes) and `addEventListener` accepts a boolean third argument for capture (`dom/node.rs:623` handles the bool form, so yes).

- [ ] **Step 4: Run new + existing event tests**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p strake-vibey-script --test capture_phase --test dom 2>&1 | grep -E "^test |test result" | head -n 30
```

Expected: all PASS, especially the pre-existing `click_events_bubble_and_stop_propagation`. If it regresses, the phase split broke bubble semantics — fix the implementation, never the old test.

- [ ] **Step 5: Commit**

```bash
git add packages/strake-vibey-script/src/runtime.rs packages/strake-vibey-script/tests/capture_phase.rs && git commit -m "feat: capture-phase event dispatch honoring listener capture flag (issue #2)"
```

### Task 8: Restyle-isolation bypass test (paint-only mutations skip Taffy)

Uses `layout_pass_count` (Task 4) and the `color_of` + `layout_rect` patterns from Tasks 5-6.

**Files:**
- Create: `tests/strake-tests/tests/restyle_layout_bypass.rs`

**Interfaces:**
- Consumes: `BaseDocument::layout_pass_count`, `set_style_property` (`document.rs:718`), `Harness`/`HtmlDocument` resolve flow.
- Produces: proof that `color`/`background-color` mutations cause zero Taffy passes while `width` causes one.

- [ ] **Step 1: Write the test**

```rust
//! Restyle-invalidation isolation pins (issue #2, section 3D): paint-only
//! style mutations must not trigger Taffy layout passes.

use std::sync::Arc;
use strake_dom::{DocumentConfig, LocalName, QualName, ns};
use strake_html::{HtmlDocument, HtmlProvider};
use strake_traits::shell::{ColorScheme, Viewport};

fn qname(local: &str) -> QualName {
    QualName {
        prefix: None,
        ns: ns!(html),
        local: LocalName::from(local),
    }
}

#[test]
fn paint_only_mutation_skips_taffy_layout() {
    let mut doc = HtmlDocument::from_html(
        r#"<!DOCTYPE html><html><head><style>
            body { margin: 0; }
            #box { width: 100px; height: 50px; color: rgb(0, 0, 0); }
        </style></head><body><div id="box">hi</div></body></html>"#,
        DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            font_ctx: Some(strake_dom::hermetic_test_font_context()),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    let baseline = doc.layout_pass_count();
    assert!(baseline >= 1);

    // Paint-only mutation: restyle must apply with zero Taffy passes.
    let node = doc.query_selector("#box").unwrap().unwrap();
    doc.set_style_property(node, "color", "rgb(0, 0, 255)");
    doc.resolve(0.0);
    assert_eq!(doc.layout_pass_count(), baseline);

    // Layout-affecting mutation: exactly one Taffy pass, geometry updated.
    doc.set_style_property(node, "width", "200px");
    doc.resolve(0.0);
    assert_eq!(doc.layout_pass_count(), baseline + 1);
    let layout = doc.get_node(node).unwrap().final_layout();
    assert_eq!(layout.size.width, 200.0);
}
```

`set_style_property` on the document is verified at `document.rs:718`; `final_layout` usage is verified in `br_trailing_line.rs`. If `resolve()` runs Taffy unconditionally today (i.e. `resolve_layout` has no damage early-out), the first new assertion FAILS — that is the test doing its job. In that case implement the minimal early-out in `resolve_layout_children`/`resolve_layout` gated on "no layout damage in tree" (see `propagate_damage_flags` return value and `incremental_layout` flag at `resolve.rs:89-93`), then re-run. Do not fake the counter.

- [ ] **Step 2: Run**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p strake-tests --test restyle_layout_bypass -- --nocapture 2>&1 | tail -n 8
```

Expected: PASS (possibly after adding the early-out described above).

- [ ] **Step 3: Commit**

```bash
git add tests/strake-tests/tests/restyle_layout_bypass.rs packages/strake-dom/src/resolve.rs && git commit -m "test: prove paint-only restyles skip Taffy layout (issue #2)"
```

(If `resolve.rs` was not modified, drop it from the `git add`.)

### Task 9: Full verification, CI matrix note, and issue update

**Files:** none (verification + `gh` only).

- [ ] **Step 1: Run every gate**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo check --workspace 2>&1 | tail -n 2 && cargo clippy --workspace -- -D warnings 2>&1 | tail -n 2 && cargo fmt --all -- --check && echo ALL_STATIC_OK && cargo test --workspace 2>&1 | grep -cE "test result: FAILED|error\[|panicked" | grep -q "^0$" && echo ALL_TESTS_OK
```

Expected: `ALL_STATIC_OK` and `ALL_TESTS_OK`. If any failure appears, fix the code (never delete/skip the failing test) and re-run.

- [ ] **Step 2: Confirm cross-platform coverage already exists**

Verify the `matrix_test` job in `.github/workflows/ci.yml` still runs `cargo test --all --tests` on windows/macos/linux (observed at lines ~127-176). The new hermetic suites therefore execute on all three OSes with zero workflow changes — state this explicitly in the issue comment. No WPT changes in this plan (curated WPT smoke + nightly cross-platform WPT matrix remain follow-up work).

- [ ] **Step 3: Comment on and close issue #2 only if everything above merged**

```bash
gh issue comment 2 --repo gregoreesmaa/strake --body "Implemented: hermetic Ahem font context default in HarnessOptions; SlotMap recycling/attribute suites; flex/grid geometry pins; capture-phase dispatch honoring the capture flag; layout_pass_count proving paint-only restyles skip Taffy. Verified: cargo check/clippy/fmt clean, cargo test --workspace green incl. windows/macos/linux matrix. Follow-ups filed separately: migrate remaining hand-rolled tests to Harness, tabindex focus-navigation suite, curated WPT smoke job."
```

Close the issue only when all tasks are merged to the default branch:

```bash
gh issue close 2 --repo gregoreesmaa/strake --reason completed
```

If any follow-up remains, leave the issue OPEN and check off completed boxes instead.

## Self-Review

1. **Spec coverage:** 3A covered by Task 5 (recycling, reparent, boolean-attribute reflection; duplicate-`id` and class-token parsing deferred — executor: add to follow-ups in Task 9 if untouched). 3B covered by Task 6 (flex auto-margin/wrap, grid `fr`/gaps; subgrid/dense-packing explicitly out of scope with reason). 3C covered by Task 7 (capture/target/bubble + both stop semantics; `tabindex` navigation and `preventDefault` default-action matrix deferred to follow-ups). 3D covered by Task 8. Checklist items "embedded fonts", "HarnessOptions default", "capture/target/bubble order", "0 Taffy passes" all have tasks; "migrate all 45 tests" and "WPT smoke/matrix" are follow-ups (stated, not silently dropped).
2. **Placeholder scan:** the `text_of_selector` body in Task 7 must be pasted from `dom.rs` at execution time (flagged inline with a read-first instruction, not left as TODO). The `Blob` import in Task 2 has a mandatory grep-first instruction with two candidate paths. No `TBD`/`handle edge cases` language remains.
3. **Type consistency:** `FontContext` comes from `strake_dom` re-export everywhere; `QualName { prefix: None, ns, local }` matches `detached_attribute.rs`; `DomEvent::new(id, synthetic_click_event(...))` matches `dom.rs`; `layout.size.width`/`final_layout()` match `br_trailing_line.rs`.

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-09-12-hermetic-headless-testing.md`. Two execution options:

**1. Subagent-Driven (recommended)** - I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** - Execute tasks in this session using executing-plans, batch execution with checkpoints

**Which approach?**




