//! N-API native-addon seam (issue #18, Phase-0).
//!
//! Issue #18 is explicitly post-MVP: no canary is blocked on the N-API ABI,
//! and the whole point of the hybrid strategy is that most apps never need
//! it. What Phase-0 owes the epic is the seam every later step builds on:
//! the required `napi_*` symbol registry (value create/inspect,
//! function/property interop, references/finalizers, async work — the
//! subset named in the issue), a loader that reports the exact missing
//! symbol instead of a generic crash (the issue's fallback contract), and
//! the manifest policy bit (default-deny, shared with issue #16). No C ABI
//! surface is implemented here; every symbol is reported unimplemented.

use std::collections::HashSet;

/// N-API call outcome codes (the stable subset the seam reports).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NapiStatus {
    /// Success.
    Ok,
    /// An argument or environment is invalid.
    InvalidArg,
    /// A native object is in the wrong state.
    InvalidState,
    /// The runtime threw a pending JS exception.
    PendingException,
    /// The requested symbol is not implemented by this runtime.
    NotImplemented,
}

/// Required `napi_*` symbols, grouped by the issue's checklist: value
/// create/inspect, function/property interop, references/finalizers, and
/// async work.
pub const REQUIRED_SYMBOLS: &[&str] = &[
    // Value create/inspect.
    "napi_create_int32",
    "napi_create_double",
    "napi_create_string_utf8",
    "napi_create_object",
    "napi_create_array",
    "napi_get_value_int32",
    "napi_get_value_double",
    "napi_get_value_string_utf8",
    "napi_typeof",
    // Function/property interop.
    "napi_create_function",
    "napi_new_instance",
    "napi_get_named_property",
    "napi_set_named_property",
    "napi_has_named_property",
    "napi_get_property_names",
    "napi_call_function",
    // References and finalizers.
    "napi_create_reference",
    "napi_delete_reference",
    "napi_get_reference_value",
    "napi_add_finalizer",
    // Async work.
    "napi_create_async_work",
    "napi_queue_async_work",
    "napi_delete_async_work",
];

/// Symbols the runtime implements today: none yet (post-MVP by design).
pub const IMPLEMENTED_SYMBOLS: &[&str] = &[];

/// What a native addon needs from the runtime.
#[derive(Debug, Clone)]
pub struct AddonRequirements {
    /// `napi_*` symbols the addon imports.
    pub symbols: Vec<String>,
}

impl AddonRequirements {
    /// Requirements for an addon importing `symbols`.
    pub fn new(symbols: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            symbols: symbols.into_iter().map(Into::into).collect(),
        }
    }
}

/// Symbols in `requirements` the runtime does not implement, in order.
pub fn missing_symbols(requirements: &AddonRequirements) -> Vec<String> {
    let implemented: HashSet<&str> = IMPLEMENTED_SYMBOLS.iter().copied().collect();
    requirements
        .symbols
        .iter()
        .filter(|symbol| !implemented.contains(symbol.as_str()))
        .cloned()
        .collect()
}

/// Native-addon load outcome (the issue's clear-diagnostic contract).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddonLoad {
    /// Every required symbol resolved.
    Ready,
    /// Loading refused: policy denied, or symbols missing (named exactly).
    Refused {
        /// Human-readable reason, naming the missing symbols.
        reason: String,
    },
}

/// Decide whether an addon loads: the manifest policy bit first
/// (default-deny, shared with issue #16), then symbol resolution. Missing
/// symbols produce the exact missing function names, never a generic crash.
pub fn check_addon_load(allow_native_addons: bool, requirements: &AddonRequirements) -> AddonLoad {
    if !allow_native_addons {
        return AddonLoad::Refused {
            reason: String::from("native addons are not permitted by the app manifest"),
        };
    }
    let missing = missing_symbols(requirements);
    if missing.is_empty() {
        AddonLoad::Ready
    } else {
        AddonLoad::Refused {
            reason: format!("unimplemented N-API symbols: {}", missing.join(", ")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sqlite_like() -> AddonRequirements {
        AddonRequirements::new([
            "napi_create_int32",
            "napi_create_function",
            "napi_create_async_work",
        ])
    }

    #[test]
    fn policy_denies_by_default() {
        assert_eq!(
            check_addon_load(false, &sqlite_like()),
            AddonLoad::Refused {
                reason: String::from("native addons are not permitted by the app manifest"),
            }
        );
    }

    #[test]
    fn diagnostics_name_the_exact_missing_symbols() {
        let loaded = check_addon_load(true, &sqlite_like());
        let AddonLoad::Refused { reason } = loaded else {
            panic!("nothing is implemented yet, expected Refused, got {loaded:?}");
        };
        for symbol in [
            "napi_create_int32",
            "napi_create_function",
            "napi_create_async_work",
        ] {
            assert!(
                reason.contains(symbol),
                "diagnostic names {symbol}: {reason}"
            );
        }
    }

    #[test]
    fn registry_covers_the_issue_checklist() {
        // Value create/inspect, function/property interop,
        // references/finalizers, async work — at least one per group.
        for group in [
            ["napi_create_int32", "napi_typeof"],
            ["napi_create_function", "napi_call_function"],
            ["napi_create_reference", "napi_add_finalizer"],
            ["napi_create_async_work", "napi_queue_async_work"],
        ] {
            for symbol in group {
                assert!(
                    REQUIRED_SYMBOLS.contains(&symbol),
                    "registry covers {symbol}"
                );
            }
        }
    }
}
