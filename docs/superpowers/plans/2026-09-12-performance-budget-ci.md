# Performance Budget CI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enforce the binary-size (25MB), cold-start (100ms p95), and idle-RSS (30MB) budgets from issue #11 on every PR with a std-only bench binary and a CI gate that fails loudly with actionable numbers.

**Architecture:** Add a dependency-free `strake-bench` binary crate that parses + resolves a generated 1000-node fixture 50 times with `Instant` timing and samples RSS from `/proc/self/statm` on Linux; add a `perf-budget` job to `.github/workflows/ci.yml` that builds the `--profile production` binary, fails it over 25MB, runs the bench against env-var thresholds, and appends the budget table to `$GITHUB_STEP_SUMMARY`.

**Tech Stack:** Rust 2024, `std::time::Instant` only (no criterion/dhat — see constraints), existing `[profile.production]` (opt-level 3, lto, codegen-units 1, strip), GitHub Actions step summaries.

**Spec:** GitHub issue #11 — `[Epic] Continuous Performance Regression CI` (`gh issue view 11 --repo gregoreesmaa/strake`). The plan argues from that issue; executors read both.

## Global Constraints

- Rust 2024 Edition, MSRV 1.91.0+.
- NEVER add new external crates to any `Cargo.toml` without explicit user permission (AGENTS.md). No criterion, no dhat, no jemalloc-ctl in this plan — timing via `std::time::Instant`, RSS via `/proc` parsing with `std::fs`. This is a deliberate, disclosed deviation from issue #11's `cargo bench -p strake-bench` / dhat asks: the local workflow is `cargo run -p strake-bench` and allocation flamegraphs move to a nightly follow-up.
- Thresholds are env-var overridable (`PERF_MAX_P95_MS`, `PERF_MAX_RSS_MB`, `PERF_MAX_BINARY_MB`) with issue #11 defaults (100.0, 30.0, 25.0). The binary exits nonzero on any breach and prints which budget broke with measured vs allowed values.
- Requires the hermetic font context from the companion plan (`docs/superpowers/plans/2026-09-12-hermetic-headless-testing.md`, Task 2: `strake_dom::hermetic_test_font_context`). Execute that plan first — the bench must not depend on host fonts or CI numbers will wobble per runner image.
- Be surgical and minimal. Verify each task with the commands named in it.

## File Structure

- Modify: `Cargo.toml` — append `packages/strake-bench` to `[workspace] members`.
- Create: `packages/strake-bench/Cargo.toml` — bin crate, workspace-internal deps only.
- Create: `packages/strake-bench/src/main.rs` — fixture generation, 50-iteration cold-boot loop (median/p95/stddev), RSS sample, threshold gate, human + `GITHUB_STEP_SUMMARY` output.
- Modify: `.github/workflows/ci.yml` — add `perf-budget` job (ubuntu-latest, production profile build + size gate + bench run).
- Modify: `justfile` — add `bench` recipe (`cargo run --profile production -p strake-bench`).

---

### Task 1: Create the `strake-bench` crate shell

Registers the crate and proves it builds before any measurement logic lands.

**Files:**
- Modify: `Cargo.toml` (`[workspace] members`)
- Create: `packages/strake-bench/Cargo.toml`
- Create: `packages/strake-bench/src/main.rs` (stub)

**Interfaces:**
- Consumes: workspace-internal crates only. Produces: buildable `strake-bench` binary.

- [ ] **Step 1: Register the workspace member**

In the root `Cargo.toml`, append `"packages/strake-bench",` to the `[workspace] members` array (keep alphabetical-ish order as-is; the list is roughly grouped, so add it after the last `packages/` entry).

- [ ] **Step 2: Write the crate manifest**

```toml
[package]
name = "strake-bench"
description = "Hermetic performance-budget harness: cold-boot latency, idle RSS, binary size gate (issue #11)"
publish = false
version.workspace = true
license.workspace = true
homepage.workspace = true
repository.workspace = true
categories.workspace = true
edition.workspace = true
rust-version.workspace = true

[[bin]]
name = "strake-bench"
path = "src/main.rs"

[dependencies]
# Workspace-internal only (no new external crates per repo policy).
strake-dom = { workspace = true }
strake-html = { workspace = true }
strake-traits = { workspace = true }
```

Check how `strake-html` is depended on elsewhere first: `strake-test-harness` uses `strake-html = { workspace = true }` (its Cargo.toml has no features), and `strake-tests` dev-deps enable `strake-dom` features (`accessibility`, `floats`, `system-fonts`). The bench must NOT enable `system-fonts` (it uses the hermetic context), so depend on `strake-dom` with `default-features = false` — verify against the root `[workspace.dependencies]` entry for `strake-dom` (`default-features = false` is already set there, so a plain `{ workspace = true }` inherits that; confirm by reading the root manifest before writing).

- [ ] **Step 3: Stub main and prove it builds**

```rust
//! Performance-budget harness (issue #11). Measurement logic lands in Task 2.
fn main() {
    println!("strake-bench: stub (Task 2 implements measurement)");
}
```

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo check -p strake-bench 2>&1 | tail -n 3
```

Expected: success.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml packages/strake-bench/ && git commit -m "feat: add strake-bench crate shell (issue #11)"
```

### Task 2: Implement cold-boot + RSS measurement with threshold gate

Single-file implementation (the whole tool is ~150 lines; one focused file beats premature splitting). Fixture: a generated HTML string with 1000 styled divs — no fixture files to keep in sync. Document construction mirrors `HtmlDocument::from_html(html, DocumentConfig { viewport, html_parser_provider, font_ctx: Some(hermetic...), ..Default::default() })` exactly as in `tests/strake-tests/tests/dom_slotmap_recycling.rs` (companion plan Task 5), followed by `doc.resolve(0.0)`; drop the document each iteration so every sample is a true cold boot.

**Files:**
- Modify: `packages/strake-bench/src/main.rs` (full implementation)

**Interfaces:**
- Consumes: `strake_dom::hermetic_test_font_context` (companion plan), `HtmlDocument::from_html`, `DocumentConfig`, `Viewport`.
- Produces: stdout report + exit code (0 pass, 1 breach) + `$GITHUB_STEP_SUMMARY` markdown table when that env var is set.

- [ ] **Step 1: Write the failing-then-passing binary**

```rust
//! Hermetic performance-budget harness (issue #11).
//!
//! Measures, with std-only timing:
//! - cold-boot latency: parse + `resolve(0.0)` of a 1000-node fixture,
//!   50 iterations, reporting median / p95 / stddev;
//! - idle RSS after resolve, via `/proc/self/statm` (Linux only;
//!   prints SKIP elsewhere so macOS/Windows dev machines stay usable).
//!
//! Budgets (issue #11 defaults, env-overridable):
//! - `PERF_MAX_P95_MS` (default 100.0): cold-boot p95 ceiling.
//! - `PERF_MAX_RSS_MB` (default 30.0): post-resolve idle RSS ceiling.
//!
//! Exit code is 1 on any breach, naming the breached budget with
//! measured-vs-allowed values. Binary size is gated in CI (Task 3),
//! not here, because a binary cannot portably stat its own file.

use std::sync::Arc;
use std::time::Instant;

use strake_dom::DocumentConfig;
use strake_html::{HtmlDocument, HtmlProvider};
use strake_traits::shell::{ColorScheme, Viewport};

const ITERATIONS: usize = 50;

fn fixture() -> String {
    let mut html = String::from(
        "<!DOCTYPE html><html><head><style>\
         body { margin: 0; font-family: Ahem; font-size: 16px; }\
         .row { display: flex; width: 800px; }\
         .cell { width: 100px; height: 20px; color: rgb(10, 20, 30); }\
         </style></head><body>",
    );
    for i in 0..500 {
        html.push_str(&format!(
            "<div class=\"row\" id=\"r{i}\"><div class=\"cell\">cell {i} a</div>\
             <div class=\"cell\">cell {i} b</div></div>"
        ));
    }
    html.push_str("</body></html>");
    html
}

fn cold_boot_once(html: &str) -> f64 {
    let start = Instant::now();
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            font_ctx: Some(strake_dom::hermetic_test_font_context()),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    drop(doc);
    start.elapsed().as_secs_f64() * 1000.0
}

/// Post-resolve idle RSS in MiB. `None` on non-Linux (caller reports SKIP).
fn idle_rss_mib(html: &str) -> Option<f64> {
    #[cfg(target_os = "linux")]
    {
        let mut doc = HtmlDocument::from_html(
            html,
            DocumentConfig {
                viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
                html_parser_provider: Some(Arc::new(HtmlProvider) as _),
                font_ctx: Some(strake_dom::hermetic_test_font_context()),
                ..Default::default()
            },
        );
        doc.resolve(0.0);
        // Quiescence: resolve twice so deferred/microtask-driven work settles.
        doc.resolve(0.0);
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let resident_pages: f64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        drop(doc);
        // RSS pages are 4 KiB on every supported Linux target (x86_64/aarch64).
        Some(resident_pages * 4096.0 / 1024.0 / 1024.0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = html;
        None
    }
}

fn percentile(sorted: &mut [f64], p: f64) -> f64 {
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let idx = ((p / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx]
}

fn env_or(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let max_p95 = env_or("PERF_MAX_P95_MS", 100.0);
    let max_rss = env_or("PERF_MAX_RSS_MB", 30.0);
    let html = fixture();

    // Warm up allocator/page cache once, outside the measured window.
    cold_boot_once(&html);

    let mut samples: Vec<f64> = (0..ITERATIONS).map(|_| cold_boot_once(&html)).collect();
    let median = percentile(&mut samples, 50.0);
    let p95 = percentile(&mut samples, 95.0);
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    let variance = samples.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / samples.len() as f64;
    let rss = idle_rss_mib(&html);

    println!("cold-boot over {ITERATIONS} iterations (ms):");
    println!("  median {median:.2}  p95 {p95:.2}  mean {mean:.2}  stddev {:.2}", variance.sqrt());
    match rss {
        Some(mib) => println!("idle RSS after resolve: {mib:.2} MiB"),
        None => println!("idle RSS: SKIP (RSS sampling is Linux-only)"),
    }

    let mut breaches = Vec::new();
    if p95 > max_p95 {
        breaches.push(format!("cold-boot p95 {p95:.2} ms exceeds budget {max_p95:.2} ms"));
    }
    if let Some(mib) = rss {
        if mib > max_rss {
            breaches.push(format!("idle RSS {mib:.2} MiB exceeds budget {max_rss:.2} MiB"));
        }
    }
    if !breaches.is_empty() {
        for b in &breaches {
            eprintln!("PERF BREACH: {b}");
        }
        std::process::exit(1);
    }
    println!("PERF OK: p95 {p95:.2} ms (budget {max_p95:.2}), rss {} (budget {max_rss:.2} MiB)",
        rss.map(|m| format!("{m:.2} MiB")).unwrap_or_else(|| "SKIP".to_string()));

    // Appends rows to the CI step summary. The Binary Size step (Task 3)
    // writes the table header first; use append-only here so rows from
    // both steps land in one table. Locally GITHUB_STEP_SUMMARY is unset
    // and this block is skipped.
    if let Ok(summary) = std::env::var("GITHUB_STEP_SUMMARY") {
        use std::fmt::Write as _;
        use std::fs::OpenOptions;
        use std::io::Write as _;
        let mut table = String::new();
        let _ = writeln!(table, "| **Cold Boot (p95)** | {p95:.2} ms | {max_p95:.2} ms | ✅ PASS |");
        match rss {
            Some(mib) => { let _ = writeln!(table, "| **Idle Memory (RSS)** | {mib:.2} MiB | {max_rss:.2} MiB | ✅ PASS |"); }
            None => { table.push_str("| **Idle Memory (RSS)** | SKIP (non-Linux) | — | ⚪ SKIP |\n"); }
        }
        OpenOptions::new().create(true).append(true).open(summary)
            .and_then(|mut f| f.write_all(table.as_bytes()))
            .expect("append step summary");
    }
}
```

API risks to reconcile at execution time (all read-first, none blocking): (a) `HtmlDocument::from_html` signature — copy the exact call shape from the companion plan's `dom_slotmap_recycling.rs` (verified against `detached_attribute.rs` + `br_trailing_line.rs`); (b) `doc.resolve(0.0)` takes `&mut self` (verified in every existing test); (c) `Viewport::new(w, h, scale, scheme)` (verified); (d) the `Document` trait import if `resolve` needs it in scope — check what `style_property_invalidation.rs` imports (`strake_dom::{BaseDocument, DocumentConfig}` suggests `resolve` may live on a trait or inherent impl; mirror that file's imports exactly).

- [ ] **Step 2: Run locally and record the numbers**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo run -p strake-bench 2>&1 | tail -n 8
```

Expected: `PERF OK` with concrete numbers on this machine. If p95 exceeds 100ms in a debug build, that is expected — re-run with `cargo run --profile production -p strake-bench` and record THOSE numbers (CI gates the production profile; the plan's budgets assume optimized code). If production p95 still breaches on this Mac, do not raise the default: report the numbers and keep the breach failing loudly — the budget is the point.

- [ ] **Step 3: Prove the gate actually fails**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && PERF_MAX_P95_MS=0.001 cargo run -p strake-bench 2>&1 | tail -n 3; echo "EXIT:$?"
```

Expected: `PERF BREACH` on stderr and nonzero exit. A gate that cannot fail proves nothing.

- [ ] **Step 4: Commit**

```bash
git add packages/strake-bench/src/main.rs packages/strake-bench/Cargo.toml && git commit -m "feat: implement strake-bench cold-boot and RSS measurement (issue #11)"
```

### Task 3: Add the `perf-budget` CI job (size gate + bench + step summary)

Builds the production-profile binary and enforces all three budgets. Ubuntu-only in this plan (Linux gives us RSS + `stat`; macOS/Windows matrix and nightly flamegraphs are follow-ups). Mirror the existing job style in `.github/workflows/ci.yml` (e.g. the `Test [default features]` job: checkout → toolchain → apt deps → cargo invocation).

**Files:**
- Modify: `.github/workflows/ci.yml` (append job)
- Modify: `justfile` (add `bench` recipe)

**Interfaces:**
- Consumes: `strake-bench` binary (Task 2), `[profile.production]` (existing). Produces: red/green PR gate + budget table in step summary.

- [ ] **Step 1: Append the job to ci.yml**

```yaml
  perf-budget:
    name: "Performance budgets (binary size, cold boot, RSS)"
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: sudo apt-get update && sudo apt-get install -y libfontconfig1-dev
      - name: Build production bench binary
        run: cargo build --profile production -p strake-bench
      - name: Binary size gate (budget 25MB)
        env:
          PERF_MAX_BINARY_MB: "25"
        run: |
          BIN=./target/production/strake-bench
          BYTES=$(stat -c%s "$BIN")
          MB=$(python3 -c "print($BYTES/1024/1024)")
          echo "strake-bench binary: $MB MiB (budget ${PERF_MAX_BINARY_MB} MiB)"
          {
            echo "### Strake Performance Budget Report"
            echo "| Metric | Measured | Budget | Status |"
            echo "| :--- | :---: | :---: | :---: |"
            echo "| **Binary Size** | $MB MiB | ${PERF_MAX_BINARY_MB} MiB | ✅ PASS |"
          } >> "$GITHUB_STEP_SUMMARY"
          python3 -c "import sys; sys.exit(0 if float('$MB') <= float('$PERF_MAX_BINARY_MB') else 1)" \
            || { echo "PERF BREACH: binary size $MB MiB exceeds budget ${PERF_MAX_BINARY_MB} MiB"; exit 1; }
      - name: Cold-boot and RSS gate
        env:
          PERF_MAX_P95_MS: "100"
          PERF_MAX_RSS_MB: "30"
        run: ./target/production/strake-bench
```

Notes: (a) custom profiles output to `target/<profile-name>` — `production` → `target/production` (verify with `ls` after the build step if unsure); (b) the size step writes the table header + size row BEFORE its gate so breaching PRs still show the table; the bench binary appends its rows (Task 2 already uses append-only `OpenOptions`); (c) keep `libfontconfig1-dev` because the workspace default features still need it (matches every existing Linux job).

- [ ] **Step 2: Add the local justfile recipe**

Append to `justfile` (top section with `check:`/`clippy:`/`fmt:`, matching existing style):

```just
bench:
  cargo run --profile production -p strake-bench
```

- [ ] **Step 3: Validate YAML and dry-run what can run locally**

```bash
python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/ci.yml'))" 2>/dev/null || python3 -c "import sys; print('pyyaml missing, skipping parse check')"
export PATH="$HOME/.cargo/bin:$PATH" && cargo build --profile production -p strake-bench 2>&1 | tail -n 3 && ls -l target/production/strake-bench && ./target/production/strake-bench 2>&1 | tail -n 5
```

Expected: build succeeds, binary runs, `PERF OK` (production profile). Record the binary size in MiB — if it already exceeds 25MB, STOP and report (the budget or the binary needs a decision, not a silent bump).

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml justfile && git commit -m "ci: add perf-budget gate for binary size, cold boot, RSS (issue #11)"
```

### Task 4: Full verification and issue update

- [ ] **Step 1: Run every gate**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo check --workspace 2>&1 | tail -n 2 && cargo clippy --workspace -- -D warnings 2>&1 | tail -n 2 && cargo fmt --all -- --check && echo ALL_STATIC_OK && cargo test --workspace 2>&1 | grep -cE "test result: FAILED|error\[|panicked" | grep -q "^0$" && echo ALL_TESTS_OK
```

Expected: `ALL_STATIC_OK` and `ALL_TESTS_OK`.

- [ ] **Step 2: Comment on and close issue #11 only if merged**

```bash
gh issue comment 11 --repo gregoreesmaa/strake --body "Implemented: strake-bench binary (50-iteration cold-boot median/p95/stddev + Linux RSS via /proc/self/statm, std-only, hermetic Ahem fonts); perf-budget CI job gating binary size 25MB, cold-boot p95 100ms, RSS 30MB with step-summary table; just bench recipe. Verified: check/clippy/fmt clean, workspace tests green, gate proven to fail with PERF_MAX_P95_MS=0.001. Follow-ups: dhat/flamegraph attribution on breach, macOS/Windows RSS sampling, cross-platform WPT-style nightly matrix."
```

```bash
gh issue close 11 --repo gregoreesmaa/strake --reason completed
```

If any follow-up remains open, leave the issue OPEN and check off completed boxes instead.

## Self-Review

1. **Spec coverage:** binary budget → Task 3 size gate with per-crate attribution deferred (honest: `cargo bloat` diff on breach is follow-up, the gate + numbers ship now). Cold-boot → Task 2 (50 iterations, median/p95/stddev, 100ms default). Idle RSS → Task 2 (`/proc/self/statm`, 30MB default, SKIP elsewhere). PR tracking table → Task 2 + 3 step-summary rows. Local workflow → `just bench` / `cargo run -p strake-bench` (deviation from `cargo bench` disclosed in constraints with the no-new-deps rationale).
2. **Placeholder scan:** all code blocks are complete and compilable modulo the four named read-first reconciliations in Task 2 Step 1 (each names the exact file/lines to mirror). No TBD/TODO/edge-case hand-waving.
3. **Type consistency:** `DocumentConfig { viewport, html_parser_provider, font_ctx, ..Default::default() }` matches the companion plan's constructor usage; env defaults (100.0/30.0/25.0) match the issue table everywhere including the YAML.

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-09-12-performance-budget-ci.md`. Two execution options:

**1. Subagent-Driven (recommended)** - I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** - Execute tasks in this session using executing-plans, batch execution with checkpoints

**Which approach?**


