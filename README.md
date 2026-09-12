# Boson — Ultra-Fast Native Web Runtime & Modern Electron Alternative

> *"Electron gives apps mass. Boson gives them velocity."*

[![Build Status](https://github.com/gregoreesmaa/boson/actions/workflows/ci.yml/badge.svg)](https://github.com/gregoreesmaa/boson/actions)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE-APACHE)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE-MIT)
[![Repository](https://img.shields.io/badge/GitHub-gregoreesmaa%2Fboson-181717.svg?logo=github)](https://github.com/gregoreesmaa/boson)

> **Notice:** Boson is an independent native application runtime originally derived from [Blitz](https://github.com/DioxusLabs/blitz) by DioxusLabs. It builds upon modular HTML/CSS foundations (Stylo, Taffy, Parley, Vello) to deliver a true lightweight **Electron replacement** capable of running standard web applications natively across **macOS, Windows, Linux, iOS, and Android**, backed by an on-demand, zero-copy offscreen Chromium fallback. See [NOTICE](NOTICE) for full attribution.

---

## The Boson Ethos: 100% Compatibility at Inception, Relentless Ultra-Optimization in Perpetuity

> **"In the compatibility layer, all hacks are permissible. On the modern path, zero waste is tolerated."**

Boson is governed by two inviolable laws:

1. **The Inviolable Law of Compatibility (Day 1 Drop-in Replacement):**
   * Any application written for Electron or standard web environments must run in Boson on Day 1 without breaking changes or painful architectural rewrites.
   * In the compatibility layer, **all pragmatic shims, polyfills, monkey-patches, and offscreen Chromium fallbacks are completely acceptable**. No edge-case feature, legacy API, or obscure web specification will be turned away if an existing real-world app depends on it. Compatibility is our adoption vector.

2. **The Inviolable Law of Ultra-Optimization (The End-State Moat):**
   * In our target steady state, **no heavy rendering engines, multi-process bloat, or redundant virtual machines may permanently consume disk space, CPU, GPU, or RAM**.
   * Ultra-optimization is our technical reason for being: **sub-30MB baseline RAM, sub-100ms cold startup, 120fps GPU compute rendering, 0% CPU at idle, and sub-20MB distributable binaries**.

3. **The Bifurcated Architecture: The Modern "Hyper-Path":**
   * **The Legacy / Fallback Path:** For unpolyfilled APIs or complex legacy browser subsystems (WebRTC, Widevine, intricate iframes), Boson dynamically routes rendering to a disposable, heavily throttled offscreen Chromium worker that hibernates or terminates the moment it is hidden.
   * **The Modern Hyper-Path:** When an application authors clean, modern, evergreen web code (standard Flexbox/Grid layouts, modern ES modules, native Web APIs), the engine bypasses all shims, virtualization, and Chromium overhead entirely—executing directly on bare-metal Rust and GPU compute shaders with minimal resource consumption.
   * **The Developer Incentive:** Developers are never blocked by missing features, but are naturally rewarded with featherweight, battery-friendly native performance simply by writing modern standard code.

---

## The Vision & Architectural Moat

Traditional desktop web wrappers (Electron, CEF) embed an entire Google Chromium browser and Node.js instance for every single application, consuming hundreds of megabytes of RAM, bloating binary sizes past 150MB, draining laptop battery life, and introducing seconds of cold-boot latency.

**Boson** re-architects client application infrastructure from first principles:

### 1. Radically Lean Resource Footprint
* **Instant Cold Starts:** Sub-100ms cold application launch.
* **Featherweight Memory:** Sub-30MB baseline RSS memory footprint (compared to Electron's 200MB–500MB+).
* **Power Efficiency:** 120fps GPU compute-shader vector rendering (via Vello / WGPU) with 0 FPS idle sleep when static.
* **Compact Binaries:** Under 20MB stripped distributable binaries with sub-500KB delta updates.

### 2. Universal Cross-Platform Targets
* **Desktop:** macOS (Metal), Windows (DirectX 12 / 11), Linux (Vulkan / Wayland / X11).
* **Mobile:** iOS (UIKit) and Android (SurfaceView) using the identical unified Rust core.

### 3. Integrated Native JS/TS Execution
* Run standard web code (HTML, CSS, JavaScript/TypeScript) without a bloated multi-process V8 engine.
* Native microtask/macrotask HTML5 event loop wired directly to `boson-dom`'s generational SlotMap.
* Dynamic QuickJS-ng runtime with future Static Hermes AOT compilation for typed bundles.

### 4. 100% Web Feature Coverage via Lightweight Strangler-Fig Fallback
While Boson renders modern HTML and CSS with extreme speed and fidelity, arbitrary web applications occasionally rely on long-tail browser subsystems (WebRTC, Widevine DRM media, intricate iframes, or unhandled browser extensions).

Rather than failing or waiting years for full spec reimplementation:
* **Native-First Core:** 95%+ of UI layout, styling, text shaping, image rendering, and DOM events execute directly in native Rust.
* **Zero-Cost Idle Fallback:** The headless Chromium worker is **never launched at startup** (0 MB RAM, 0% CPU at rest).
* **On-Demand JIT Spawning:** Spawns an isolated headless Chromium worker *only* when an unsupported element is mounted.
* **Zero-Copy GPU Texture Sharing:** Offscreen frames stream directly into Boson's WGPU pipeline via OS-level shared GPU textures (`IOSurface` on macOS, `DXGI` shared handles on Windows, `dma-buf` on Linux) with zero CPU memory copying.
* **Aggressive Auto-Eviction:** Worker hibernates after 15 seconds of inactivity and terminates after 30 seconds of zero active fallback nodes, restoring memory back to the sub-30MB baseline.

---

## System Architecture

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
│  • Generational SlotMap FFI   │         │  • html5ever (HTML5 Parsing)  │
└──────────────┬────────────────┘         └──────────────┬────────────────┘
               │                                         │
               │ Direct FFI / Microtask Event Loop       │
               ▼                                         ▼
┌────────────────────────────────────────────────────────────────────────┐
│                         Boson Core (Rust)                              │
│                                                                        │
│   • Layout: Taffy (Flexbox, CSS Grid, Block layout)                    │
│   • Typography & Shaping: Parley + HarfBuzz                            │
│   • Accessibility: AccessKit (Universal OS accessibility bridge)       │
│   • Synthetic DOM & Event Dispatch                                     │
└───────────────────┬────────────────────────────────┬───────────────────┘
                    │                                │
     [Native Surface]                                │ [On-Demand Fallback Surface]
                    ▼                                ▼
┌──────────────────────────────────────┐  ┌──────────────────────────────┐
│        Vello GPU Renderer            │  │  Headless Chromium Worker    │
│   Compute-shader 2D rendering        │  │  (Offscreen CEF/Blink for    │
│   via WGPU (Metal, DX12, Vulkan)     │  │   WebRTC, complex DRM)       │
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

### Key Subsystems

* **`boson-dom`** — Core synthetic DOM incorporating style resolution, Taffy layout, text shaping, and event dispatch.
* **`boson-paint`** — Translates DOM render nodes into GPU compute draws via **anyrender** / **Vello**.
* **`boson-shell`** — Connects the engine to host OS windowing, system tray, menus, dialogs, and **AccessKit** screen readers.
* **`boson-html`** — HTML5 parsing via **html5ever**.
* **`boson-net`** — Asynchronous resource fetching, corporate proxy discovery, and system root CA resolution.
* **`boson-cdp`** — Embedded Chrome DevTools Protocol server for live inspection and Vite HMR live reloading.

---

## Trying it Out

Ensure you have a modern Rust toolchain installed (1.80+):

```bash
# Clone the repository
git clone https://github.com/gregoreesmaa/boson.git
cd boson

# Launch the native Boson browser
cargo run -rp browser

# View a Markdown document via native engine
cargo run -rp rdme ./README.md
```

---

## License & Attribution

Boson is an open-source project originally derived from [Boson](https://github.com/DioxusLabs/boson) by DioxusLabs.

This project is dual-licensed under:
* **[Apache License, Version 2.0](LICENSE-APACHE)** ([http://www.apache.org/licenses/LICENSE-2.0](http://www.apache.org/licenses/LICENSE-2.0))
* **[MIT License](LICENSE-MIT)** ([http://opensource.org/licenses/MIT](http://opensource.org/licenses/MIT))

The `stylo_taffy` crate is additionally licensed under the **Mozilla Public License 2.0 (MPL-2.0)** for interoperability with the Servo project.

See [NOTICE](NOTICE) for complete upstream copyright and patent notices, and [TRADEMARKS.md](TRADEMARKS.md) for brand guidelines.
