//! Custom protocol privilege registry
//! (`protocol.registerSchemesAsPrivileged`, issue #155).
//!
//! Records `{ scheme, privileges }` declarations so main-process bundles
//! can declare privileged custom schemes (`joplin-content`,
//! `joplin-plugin`) at load. Enforcement against actual protocol handlers
//! is follow-up work; the recording is real and observable.

/// One privileged scheme declaration: the scheme plus the names of the
/// privileges enabled for it (`standard`, `secure`, `supportFetchAPI`,
/// ...). Names pass through unvalidated — unknown future flags must not
/// break registration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivilegedScheme {
    /// Custom scheme, e.g. `"joplin-content"`.
    pub scheme: String,
    /// Enabled privilege names.
    pub privileges: Vec<String>,
}

/// Registry backing `protocol.registerSchemesAsPrivileged`.
#[derive(Clone, Debug, Default)]
pub struct ProtocolRegistry {
    schemes: Vec<PrivilegedScheme>,
}

impl ProtocolRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record privileged schemes, replacing any earlier declaration for
    /// the same scheme name (Electron's last-registration-wins stance for
    /// repeated calls).
    pub fn register_schemes(&mut self, schemes: Vec<PrivilegedScheme>) {
        for scheme in schemes {
            if let Some(slot) = self.schemes.iter_mut().find(|s| s.scheme == scheme.scheme) {
                *slot = scheme;
            } else {
                self.schemes.push(scheme);
            }
        }
    }

    /// Declared schemes, in registration order.
    pub fn privileged_schemes(&self) -> &[PrivilegedScheme] {
        &self.schemes
    }
}

#[test]
fn registering_schemes_records_them_in_order() {
    let mut registry = ProtocolRegistry::new();
    assert!(registry.privileged_schemes().is_empty());
    registry.register_schemes(vec![
        PrivilegedScheme {
            scheme: String::from("joplin-content"),
            privileges: vec![String::from("standard"), String::from("secure")],
        },
        PrivilegedScheme {
            scheme: String::from("joplin-plugin"),
            privileges: vec![String::from("standard")],
        },
    ]);
    assert_eq!(
        registry
            .privileged_schemes()
            .iter()
            .map(|s| s.scheme.as_str())
            .collect::<Vec<_>>(),
        vec!["joplin-content", "joplin-plugin"]
    );
}

#[test]
fn re_registering_a_scheme_replaces_it_in_place() {
    let mut registry = ProtocolRegistry::new();
    registry.register_schemes(vec![PrivilegedScheme {
        scheme: String::from("joplin-content"),
        privileges: vec![String::from("standard")],
    }]);
    registry.register_schemes(vec![PrivilegedScheme {
        scheme: String::from("joplin-content"),
        privileges: vec![String::from("secure")],
    }]);
    assert_eq!(
        registry.privileged_schemes(),
        &[PrivilegedScheme {
            scheme: String::from("joplin-content"),
            privileges: vec![String::from("secure")],
        }]
    );
}
