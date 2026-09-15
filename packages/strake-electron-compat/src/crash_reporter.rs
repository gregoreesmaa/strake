//! `crashReporter` recorder (webtorrent-desktop real boot).
//!
//! Day-1 stance: real minidump upload (Crashpad/Breakpad plus transport) is
//! a fallback-surface follow-up; what every Electron app needs at boot is
//! `crashReporter.start(options)` succeeding plus an honest
//! extra-parameter table (`addExtraParameter` / `getParameters`, which
//! Sentry-style integrations touch at load). So `start` records its
//! options and marks the reporter started — observable and upload-free,
//! never a silent no-op that drops what the app configured.

use std::collections::HashMap;

/// Options recorded from `crashReporter.start(options)`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CrashOptions {
    /// `productName` (the crash-report key prefix).
    pub product_name: String,
    /// `companyName`.
    pub company_name: String,
    /// `submitURL` (upload endpoint; nothing uploads in this MVP).
    pub submit_url: String,
    /// Whether compressed uploads were requested.
    pub compress: bool,
    /// `globalExtra` pairs handed to `start`.
    pub extra: HashMap<String, String>,
}

/// Headless `crashReporter`: records `start` and the extra-parameter table.
#[derive(Debug, Clone, Default)]
pub struct CrashReporter {
    started: bool,
    options: CrashOptions,
}

impl CrashReporter {
    /// A reporter that has not started.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `start` ran.
    pub fn is_started(&self) -> bool {
        self.started
    }

    /// Options from the last `start` call.
    pub fn options(&self) -> &CrashOptions {
        &self.options
    }

    /// Record a `start` (upload stays a follow-up; see module docs).
    pub fn start(&mut self, options: CrashOptions) {
        self.options = options;
        self.started = true;
    }

    /// `addExtraParameter(key, value)`.
    pub fn add_extra_parameter(&mut self, key: String, value: String) {
        self.options.extra.insert(key, value);
    }

    /// `removeExtraParameter(key)`.
    pub fn remove_extra_parameter(&mut self, key: &str) {
        self.options.extra.remove(key);
    }

    /// `getParameters()`: snapshot of the extra table.
    pub fn parameters(&self) -> HashMap<String, String> {
        self.options.extra.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_records_options_and_marks_started() {
        let mut reporter = CrashReporter::new();
        assert!(!reporter.is_started());
        let mut extra = HashMap::new();
        extra.insert("_companyName".to_string(), "WebTorrent".to_string());
        reporter.start(CrashOptions {
            product_name: "WebTorrent".to_string(),
            company_name: "WebTorrent, LLC".to_string(),
            submit_url: "https://example.invalid/crash".to_string(),
            compress: true,
            extra,
        });
        assert!(reporter.is_started());
        let options = reporter.options();
        assert_eq!(options.product_name, "WebTorrent");
        assert_eq!(options.company_name, "WebTorrent, LLC");
        assert_eq!(options.submit_url, "https://example.invalid/crash");
        assert!(options.compress);
        assert_eq!(
            options.extra.get("_companyName").map(String::as_str),
            Some("WebTorrent")
        );
    }

    #[test]
    fn extra_parameters_round_trip() {
        let mut reporter = CrashReporter::new();
        reporter.start(CrashOptions::default());
        reporter.add_extra_parameter("sentry".to_string(), "on".to_string());
        assert_eq!(
            reporter.parameters().get("sentry").map(String::as_str),
            Some("on")
        );
        reporter.remove_extra_parameter("sentry");
        assert!(reporter.parameters().is_empty());
    }
}
