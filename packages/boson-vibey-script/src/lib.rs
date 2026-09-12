//! JavaScript execution on top of Boson
//!
//! This crate implements a [`ScriptDocument`]: a wrapper around a [`BaseDocument`](boson_dom::BaseDocument)
//! which can execute the JavaScript contained in (or referenced by) the document's `<script>` tags
//! using the [Boa](https://boajs.dev) JavaScript engine, and which exposes JavaScript DOM APIs
//! (`document`, elements, events, timers, etc) backed by `boson-dom` to the scripts it runs.
//!
//! It is capable of running real-world JavaScript frameworks such as [Preact](https://preactjs.com/).
//!
//! ### Example
//!
//! ```rust
//! use boson_vibey_script::ScriptDocument;
//! use boson_dom::DocumentConfig;
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

mod clock;
mod document;
mod dom;
mod event_handler;
mod fetch;
mod runtime;
mod state;
mod timers;

pub use document::ScriptDocument;
pub use fetch::{DefaultScriptFetcher, FetchError, ScriptFetcher};
