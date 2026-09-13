//! JavaScript execution on top of Strake
//!
//! This crate implements a [`ScriptDocument`]: a wrapper around a [`BaseDocument`](strake_dom::BaseDocument)
//! which can execute the JavaScript contained in (or referenced by) the document's `<script>` tags
//! using the [Boa](https://boajs.dev) JavaScript engine, and which exposes JavaScript DOM APIs
//! (`document`, elements, events, timers, etc) backed by `strake-dom` to the scripts it runs.
//!
//! It is capable of running real-world JavaScript frameworks such as [Preact](https://preactjs.com/).
//!
//! ### Example
//!
//! ```rust
//! use strake_vibey_script::ScriptDocument;
//! use strake_dom::DocumentConfig;
//!
//! let mut doc = ScriptDocument::from_html(
//!     r#"
//!         <div id="root"></div>
//!         <script>
//!             const el = document.createElement("h1");
//!             el.textContent = "Hello from JS";
//!             document.getElementById("root").appendChild(el);
//!         </script>
//!     "#,
//!     DocumentConfig::default(),
//! );
//! doc.execute_scripts();
//! ```

#![allow(clippy::collapsible_if)]

mod app_boot;
mod clock;
mod document;
mod dom;
pub mod electron;
mod engine;
mod event_handler;
mod fetch;
mod keyword_arrow;
mod paint;
mod runtime;
mod state;
mod timers;

pub use app_boot::{
    AppBootReport, BootError, BootedWindow, IpcProof, boot_app_dir, boot_app_dir_with_ipc_proof,
};
pub use document::ScriptDocument;
pub use electron::ElectronHost;
pub use engine::ScriptEngine;
pub use fetch::{DefaultScriptFetcher, FetchError, ScriptFetcher};
pub use paint::{PaintError, PaintedAppWindow, paint_app_window};
