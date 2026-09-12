//! Strake: Ultra-fast, lightweight native application runtime and Electron alternative in Rust.
//!
//! See <https://github.com/gregoreesmaa/strake> for documentation and development roadmap.

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
