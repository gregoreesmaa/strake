<p>
<picture >
  <source media="(prefers-color-scheme: dark)" srcset="https://blitz-website.fly.dev/static/blitz-logo-with-text3-white.svg">
  <img height="70" alt="Blitz" src="https://blitz-website.fly.dev/static/blitz-logo-with-text3.svg">
</picture>
</p>

# Blitz — Next-Gen Native Web Runtime & Electron Alternative

[![Build Status](https://github.com/dioxuslabs/blitz/actions/workflows/ci.yml/badge.svg)](https://github.com/dioxuslabs/blitz/actions)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE-APACHE)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE-MIT)

> **Notice:** This repository is an independent fork of [DioxusLabs/blitz](https://github.com/DioxusLabs/blitz). It builds on the modular HTML/CSS foundations of Blitz to pursue a full, lightweight **Electron alternative** capable of running standard web applications natively across **macOS, Windows, Linux, iOS, and Android**, paired with an offscreen Chromium fallback for progressive compatibility.

---

## The Vision & Goals

Traditional desktop web wrappers (Electron, CEF) embed an entire Google Chromium browser and Node.js instance for every single application, consuming hundreds of megabytes of RAM, bloating binary sizes past 150MB, draining battery life, and introducing seconds of cold-boot latency.

This project re-architects Blitz to serve as a **high-performance, universal native runtime for standard web apps** by marrying a lightweight Rust-based native core with a **Strangler-Fig Chromium fallback**:

### 1. Radically Low Resource Footprint
* **Cold Starts:** Sub-100ms instantaneous application launches.
* **Idle Memory:** 15MB – 35MB base memory footprint instead of Electron's 150MB – 350MB+.
* **Power Efficiency:** Render strictly on demand using modern GPU compute shaders (via Vello / WGPU) rather than burning continuous CPU cycles.
* **Compact Binaries:** Under 20MB distributable binaries.

### 2. Universal Cross-Platform Targets
* **Desktop:** macOS (Metal), Windows (DirectX 12), Linux (Vulkan/Wayland/X11).
* **Mobile:** iOS and Android using the same unified Rust core and native windowing abstractions.

### 3. Integrated JS/TS Execution
* Bring standard web code (HTML, CSS, JavaScript/TypeScript) into Blitz without requiring a heavy V8 runtime.
* Incorporate high-efficiency execution tiers: Ahead-of-Time compilation for typed TypeScript/JS (via Static Hermes/Wasm) and an embedded lightweight engine (such as QuickJS-ng) for dynamic scripting.

### 4. 100% Web Feature Coverage via Strangler-Fig Fallback
While Blitz renders HTML and CSS with extreme speed and fidelity using modern Rust engines, arbitrary web applications often require long-tail browser APIs (WebRTC, hardware DRM media, complex WebGL shaders, or intricate Web Audio graphs).

Rather than failing on these APIs or waiting years for full spec reimplementation:
* **Native-First Path:** The vast majority of UI layout, styling, text, image rendering, and DOM events are handled directly in native code via Blitz.
* **Offscreen Chromium Fallback:** When an application invokes unsupported or heavy browser subsystems, a headless, offscreen Chromium worker (via CEF / Blink) renders that specific view offscreen.
* **Zero-Copy Hardware Compositing:** Fallback frames are shared directly between Chromium and Blitz using OS-level shared GPU textures (`IOSurface` on macOS, `DXGI` shared handles on Windows, `dma-buf` on Linux/Android) and composited seamlessly into the Blitz scene at display refresh rate.
* **Progressive Discarding:** As native modules for Canvas, Media, and WebGL mature within Blitz, reliance on the Chromium fallback is phased out incrementally.

---

## Core Architecture

Blitz avoids the bloat of traditional browser engines by assembling modular, best-in-class systems components:

```
┌────────────────────────────────────────────────────────────────────────┐
│                        User Web App (TS, HTML, CSS)                     │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │
       ┌────────────────────────────┴────────────────────────────┐
       ▼                                                         ▼
┌───────────────────────────────┐         ┌───────────────────────────────┐
│     JavaScript / TS Layer     │         │       UI & Styling Layer      │
│  • Static Hermes (AOT TS/JS)  │         │  • Stylo (Servo CSS Engine)   │
│  • QuickJS-ng (Dynamic JS)    │         │  • html5ever (HTML5 Parsing)  │
└──────────────┬────────────────┘         └──────────────┬────────────────┘
               │                                         │
               │ Direct FFI / IPC                        │
               ▼                                         ▼
┌────────────────────────────────────────────────────────────────────────┐
│                         Blitz Core (Rust)                              │
│                                                                        │
│   • Layout: Taffy (Flexbox, CSS Grid, Block layout)                    │
│   • Typography & Shaping: Parley + HarfBuzz                            │
│   • Accessibility: AccessKit (Universal OS accessibility bridge)       │
│   • Synthetic DOM & Event Dispatch                                     │
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

### Key Subsystems

* **`blitz-dom`** — The core DOM abstraction incorporating style resolution, layout, and event handling.
  * Powered by **[Stylo](https://github.com/servo/stylo)** (CSS parsing/resolution from Servo/Firefox), **[Taffy](https://github.com/DioxusLabs/taffy)** (box-level Flexbox and CSS Grid), and **[Parley](https://github.com/linebender/parley)** (advanced text shaping and typography).
* **`blitz-paint`** — Translates the `blitz-dom` tree into 2D drawing abstractions (**anyrender** / **Vello**).
* **`blitz-shell`** — Connects the Blitz engine to the operating system's native windowing and input pipelines via **[winit](https://github.com/rust-windowing/winit)** and **[AccessKit](https://github.com/AccessKit/accesskit)** for native screen-reader support.
* **`blitz-html`** — HTML/XHTML parser integration via **html5ever** and **xml5ever**.
* **`blitz-net`** — Asynchronous resource fetching across HTTP/HTTPS, local file systems, and encoded data URIs.

---

## Roadmap & Planned Milestones

- [x] High-performance CSS resolution via Stylo.
- [x] Modern Flexbox and CSS Grid layout via Taffy.
- [x] GPU-accelerated rendering through WGPU / Vello.
- [ ] **Dual-Engine Compositor**: Zero-copy shared GPU texture pipeline (`IOSurface` / `DXGI` / `dma-buf`) to host offscreen Chromium fallback surfaces.
- [ ] **JavaScript / TypeScript Runtime**: Embedded QuickJS-ng and Hermes integration wired directly to the Blitz synthetic DOM.
- [ ] **Web API Polyfill Bridge**: Direct OS-level implementations of `fetch`, Web Storage, Timers, FileSystem, and Clipboard.
- [ ] **Canvas 2D Engine**: Native hardware-accelerated `<canvas>` context backed by Vello.
- [ ] **Mobile Portability**: First-class packaging harnesses for iOS and Android.

---

## Trying it Out

Ensure you have a recent Rust toolchain installed:

```bash
# Clone the repository
git clone https://github.com/gregoreesmaa/blitz.git
cd blitz

# Run the Browser UI demo
cargo run -rp browser

# Run the Markdown reader
cargo run -rp rdme ./README.md

# Run the TodoMVC demo
cargo run -rp todomvc
```

---

## License & Attribution

This project is an open-source derivative of [Blitz](https://github.com/DioxusLabs/blitz), created by the DioxusLabs team and contributors.

In accordance with upstream licensing, this project is dual-licensed under:
* **[Apache License, Version 2.0](LICENSE-APACHE)** ([http://www.apache.org/licenses/LICENSE-2.0](http://www.apache.org/licenses/LICENSE-2.0))
* **[MIT License](LICENSE-MIT)** ([http://opensource.org/licenses/MIT](http://opensource.org/licenses/MIT))

The `stylo_taffy` crate is additionally licensed under the **Mozilla Public License 2.0 (MPL-2.0)** for seamless interoperability with the Servo project.

All original copyright, patent, trademark, and attribution notices from the upstream source have been retained. Modifications and extensions made in this repository are documented via commit history and distributed under the same dual Apache-2.0 / MIT terms.
