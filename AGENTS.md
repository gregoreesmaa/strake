# AGENTS.md — Autonomous Agent Operational Guide & Architecture Contract

> **Target Audience**: Autonomous AI coding agents (Antigravity, Claude Code, Cursor, Copilot Workspace, Devin).  
> **Repository**: `gregoreesmaa/boson` (**Boson**).  
> **Rule of Precedence**: Instructions in this document supersede general world knowledge or default assumptions regarding repository conventions, architectural patterns, and workflow commands.

---

## 1. Operational Directives & Agent Mindset

1. **Be Surgical & Minimal**: Touch only files directly relevant to the assigned task. Never perform unrelated refactorings or reformat unedited files.
2. **Verify Before Declaring Done**: Every code change MUST be verified locally via `cargo check --workspace` and relevant crate tests before reporting completion.
3. **No Speculative Dependencies**: NEVER add new external crates to `Cargo.toml` without explicit user permission or an established architectural mandate.
4. **Non-Interactive Execution**: NEVER run interactive commands (e.g. `cargo run -rp browser` without exit flags) in automated scripts; desktop windows block execution indefinitely.
5. **Preserve Soundness**: Zero tolerance for unvalidated `unsafe` blocks or hidden panics (`.unwrap()`) in core library crates.

---

## 2. Project Vision & North Star

### Mission
Boson is a modular, ultra-fast native HTML/CSS/JS runtime and next-generation **Electron alternative** engineered in Rust. It compiles and renders standard web code into true native desktop and mobile applications without embedding a monolithic Chromium browser or Node.js runtime.

### Target Performance Profile
* **Cold Starts**: Sub-100ms instantaneous application launches.
* **Idle Memory**: 15MB – 35MB base memory footprint (compared to Electron's 150MB – 350MB+).
* **Binary Size**: Sub-20MB distributable binaries.
* **Power Efficiency**: GPU compute-driven rendering on demand (Vello/WGPU) avoiding continuous CPU burn.
* **5-Platform Portability**: Native desktop (macOS Metal, Windows DX12, Linux Vulkan) and mobile (iOS Metal, Android Vulkan).

### The Strangler-Fig Chromium Fallback Strategy
Rather than compromising startup speed or waiting years for 100% web spec reimplementation:
* **Native-First Path**: 80%+ of typical web app surfaces (DOM, CSS Flexbox/Grid, typography, vector paint, event dispatch) are executed natively by the Rust core.
* **Offscreen Fallback**: Long-tail unhandled browser APIs (WebRTC, Widevine DRM, complex WebGL/WebGPU shaders) are lazily rendered by an offscreen headless Chromium/CEF worker.
* **Zero-Copy Hardware Compositing**: Fallback frames share native OS GPU surfaces (`IOSurface` on macOS, `DXGI` handles on Windows, `dma-buf` on Linux/Android) and composite directly into Boson's WGPU pipeline.
* **Progressive Discarding**: Chromium dependencies are systematically phased out as native Rust modules mature.

### The Boson Ethos: Compatibility First, Ultra-Optimization Always
Any autonomous agent or engineer contributing to this repository must uphold two inviolable laws:

1. **The Inviolable Law of Compatibility (Day 1 Drop-in Replacement):**
   * Boson exists to replace Electron. Existing Electron and standard web applications must run in Boson on Day 1 without requiring code rewrites.
   * In the compatibility layer, **all pragmatic hacks, shims, polyfills, monkey-patches, and offscreen Chromium fallbacks are completely acceptable**. If an obscure API or legacy behavior is needed by real-world apps, provide it without hesitation. Compatibility is our adoption vector.

2. **The Inviolable Law of Ultra-Optimization (The End-State Moat):**
   * In the target steady state, **no heavy rendering engines, multi-process bloat, or redundant virtual machines may permanently consume disk space, CPU, GPU, or RAM**.
   * Ultra-optimization is non-negotiable: **sub-30MB baseline RAM, sub-100ms cold boots, 120fps GPU compute rendering, 0% CPU at idle, and sub-20MB binaries**.

3. **The Bifurcated Execution Path:**
   * **The Fallback / Legacy Safety Net:** Unpolyfilled APIs or complex legacy browser subsystems (WebRTC, Widevine, intricate iframes) lazily trigger disposable, heavily throttled offscreen Chromium surfaces that hibernate or terminate when not visible.
   * **The Modern Hyper-Path:** When applications author modern, clean, evergreen web code (standard Flexbox/Grid, typed JS/ES modules, native Web APIs), the engine bypasses all shims, virtualization, and Chromium overhead entirely—executing directly on bare-metal Rust and GPU compute shaders with minimal resource usage.
   * **Agent Obligation:** Never break backward compatibility in the name of optimization, and never accept permanent runtime bloat in the name of convenience.

---

## 3. System Architecture & Subsystems Map

```
┌────────────────────────────────────────────────────────────────────────┐
│                        User Web App (TS, HTML, CSS)                     │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │
       ┌────────────────────────────┴────────────────────────────┐
       ▼                                                         ▼
┌───────────────────────────────┐         ┌───────────────────────────────┐
│     JavaScript / TS Layer     │         │       UI & Styling Layer      │
│  • QuickJS-ng / Static Hermes │         │  • Stylo (Servo CSS Engine)   │
│  • Generational NodeId FFI    │         │  • html5ever (HTML5 Parsing)  │
└──────────────┬────────────────┘         └──────────────┬────────────────┘
               │                                         │
               │ Direct FFI / IPC                        │
               ▼                                         ▼
┌────────────────────────────────────────────────────────────────────────┐
│                         Core Engine (Rust)                             │
│                                                                        │
│   • In-Memory DOM: SlotMap<NodeKey, Node> with 64-bit NodeId           │
│   • Style Flush: stylo_taffy bridges ComputedValues to Taffy           │
│   • Layout: Taffy (Flexbox, CSS Grid, Block layout)                    │
│   • Typography & Shaping: Parley + HarfBuzz                            │
│   • Accessibility: AccessKit (Universal OS accessibility bridge)       │
│   • Synthetic Event Loop: Macrotask queue + Microtask checkpoints       │
└───────────────────┬────────────────────────────────┬───────────────────┘
                    │                                │
     [Native Surface]                                │ [Fallback Surface]
                    ▼                                ▼
┌──────────────────────────────────────┐  ┌──────────────────────────────┐
│        Vello GPU Renderer            │  │  Headless Chromium Worker    │
│   Compute-shader 2D rendering        │  │  (Offscreen CEF/Blink for    │
│   via WGPU (Metal, DX12, Vulkan)     │  │   WebRTC, complex WebGL)     │
└───────────────────┬──────────────────┘  └──────────────┬───────────────┘
                    │                                    │
                    │ Zero-copy GPU Texture Bridge       │
                    └─────────────────┬──────────────────┘
                                      ▼
┌────────────────────────────────────────────────────────────────────────┐
│                      Winit Native Window & Events                      │
│                (macOS, iOS, Windows, Android, Linux)                   │
└────────────────────────────────────────────────────────────────────────┘
```

### Document Resolution Pipeline (`BaseDocument::resolve`)
Each tick or frame follows strict deterministic stages:
1. **Message Ingestion**: Receive async network responses, stylesheet links, image assets.
2. **Critical Resource Gate**: Halt layout if render-blocking stylesheets are pending.
3. **Device Flush**: Update viewport dimensions, zoom factor, DPI scale, light/dark color scheme.
4. **Style Resolution (Stylo)**: Compute CSS cascade and selector matching into `ComputedValues`.
5. **Damage Propagation**: Traverse dirty flags; identify layout vs paint-only mutations.
6. **Layout Tree Construction**: Generate anonymous block boxes, table wrappers, and pseudo-elements (`::before`, `::after`).
7. **Taffy Layout Pass**: Compute Flexbox, Grid, and Block bounds; Parley computes inline text shaping.
8. **Transform Resolution**: Compute CSS transforms into `kurbo::Affine` matrices.
9. **Paint Command Generation**: Emit vector commands into `anyrender::PaintScene`.

---

## 4. Repository Layout & Crate Map

The repository is structured as a Cargo workspace:

```
boson/
├── apps/
│   ├── browser/          # Desktop reference browser (bin: boson / boson)
│   │   └── persistence/  # SQLite browser history persistence (rusqlite)
│   ├── bump/             # Workspace semantic release and version bumper tool
│   └── readme/           # Standalone live-watching markdown viewer (bin: rdme)
├── packages/
│   ├── boson/            # High-level entrypoint and umbrella facade
│   ├── boson-dom/        # Headless DOM, NodeTree, Stylo/Taffy bridge, events
│   ├── boson-paint/      # Translates DOM scenes into anyrender draw commands
│   ├── boson-shell/      # Winit windowing, IME, native clipboard, file dialogs
│   ├── boson-html/       # HTML5/XHTML parser integration (html5ever, xml5ever)
│   ├── boson-net/        # Async HTTP client, streaming responses, disk cache
│   ├── boson-traits/     # Fundamental shared types (NodeId, UiEvent, Viewport)
│   ├── boson-test-harness/ # Headless test harness for DOM, layout, and events
│   ├── boson-vibey-script/ # JavaScript runtime integration (Boa -> QuickJS-ng)
│   ├── dioxus-native/    # Dioxus reactive UI integration (moving to adapter)
│   ├── dioxus-native-dom/# Headless core connecting Dioxus VDOM to boson-dom
│   ├── stylo_taffy/      # Stylo ComputedValues to Taffy style bridge (MPL-2.0)
│   ├── accesskit_xplat/  # Cross-platform AccessKit OS accessibility bridge
│   └── debug_timer/      # Zero-overhead compile-time profiling timers
├── tests/
│   └── boson-tests/      # 45 integration test suites (DOM, layout, events)
└── wpt/
    └── runner/           # Web Platform Tests (WPT) headless reftest runner
```

---

## 5. Tech Stack & Environment Prerequisites

* **Rust Toolchain**: Rust 2024 Edition, MSRV **1.91.0+**.
* **System Build Dependencies**:
  * **All OS**: Python 3 (mandatory for Stylo build-time code generation), C/C++ compiler.
  * **macOS**: Xcode Command Line Tools (`clang`, Metal SDK).
  * **Linux**: `pkg-config`, `libfontconfig1-dev`, `libssl-dev`. Runtime requires `wayland`, `libxkbcommon`, `vulkan-loader`, `libx11`.
  * **Windows**: Visual Studio 2022 C++ Build Tools (MSVC), Windows 10/11 SDK.
* **Nix Support**: Pure reproducible shell via `nix develop` (`flake.nix`).

---

## 6. Canonical Developer Workflows & Commands

All commands are executed from the repository root.

### A. Fast Compilation & Type Checks
```bash
# Check entire workspace (fastest inner loop)
cargo check --workspace

# Check core DOM package
cargo check -p boson-dom

# Check with just
just check
```

### B. Code Quality & Formatting
```bash
# Lint entire workspace with strict warnings
cargo clippy --workspace -- -D warnings
just clippy

# Format check
cargo fmt --all -- --check

# Auto-format
cargo fmt --all
just fmt
```

### C. Testing Suites
```bash
# Run all unit tests
cargo test --workspace

# Run integration tests
cargo test -p boson-tests

# Run a single integration test with stdout logging
cargo test -p boson-tests --test style_property_invalidation -- --nocapture

# Run headless test harness
cargo test -p boson-test-harness
```

### D. Running Applications & Demos
```bash
# Launch reference browser UI
just browser
# Or direct cargo command:
cargo run -rp browser --features hybrid,cookies,cache

# Launch markdown reader on a local file
just open ./README.md
# Or in pure CPU mode:
just opencpu ./README.md

# Run TodoMVC demo
just todomvc

# Run 7GUIs benchmark app
just seven_guis
```

### E. Web Platform Tests (WPT)
```bash
# Run CSS and SVG WPT reftests
just wpt css svg
# Or directly via cargo:
cargo run -rp wpt css svg
```

---

## 7. Systems & Rust Engineering Standards

### 1. Error Handling Policy
* **Library crates (`packages/*`)**: MUST use typed, explicit errors using `thiserror`. Never expose `anyhow::Result` or `Box<dyn Error>` from public library APIs.
* **Applications & Tools (`apps/*`, `wpt/*`)**: May use `anyhow` or `color-eyre` for ergonomic reporting.
* **Zero Hidden Panics**: Avoid `.unwrap()` and `.expect()` in library crates. Use proper error bubbling (`?`) or pattern matching. If mathematically impossible to fail, document with `// SAFETY: <reason>`.

### 2. Generational Memory Safety & DOM Handles
* DOM nodes are indexed via `NodeId` backed by `SlotMap<NodeKey, Node>` with a 32-bit generation and 32-bit slot index.
* Never store raw Rust references (`&Node`) across event boundaries or script execution turns.
* Any JavaScript wrapper must validate that the generational version in the `SlotMap` matches before reading/mutating node state.

### 3. Restyle & Invalidation Isolation
* When modifying styling properties, distinguish strictly between **Layout Damage** (`RELAYOUT`) and **Paint-Only Damage** (`REPAINT`).
* Mutations to `color`, `background-color`, `box-shadow`, or `cursor` MUST NEVER invalidate Taffy layout caches or trigger `compute_root_layout()`.

### 4. Unsafe Code Rules
* Every `unsafe` block MUST include an explanatory `// SAFETY:` comment proving why invariants are preserved.
* Only allow `unsafe` for FFI boundaries (Stylo, QuickJS, OS shared textures) or verified zero-copy primitives.

---

## 8. Agent Safety Rules & Boundary Constraints

### Absolute Prohibitions (NEVER DO)
* **NEVER** edit `Cargo.lock` manually; let Cargo manage lockfile updates.
* **NEVER** commit secret keys, credentials, or personal access tokens.
* **NEVER** modify `.git/` files directly.
* **NEVER** suppress lints with `#![allow(...)]` or `#[allow(...)]` at the file or crate level to silence warnings.
* **NEVER** run destructive git commands (`git reset --hard`, `git clean -fd`, `git push --force`).
* **NEVER** launch GUI binaries (`apps/browser`) in background test scripts without headless/exit flags.

### Verification Checklist Before Completing Any Task
1. [ ] `cargo check --workspace` passes with 0 errors.
2. [ ] `cargo clippy --workspace -- -D warnings` emits 0 warnings.
3. [ ] `cargo fmt --all -- --check` reports clean.
4. [ ] Relevant crate tests pass (`cargo test -p <modified-crate>`).
5. [ ] No trademark violations or unverified external dependencies introduced.
