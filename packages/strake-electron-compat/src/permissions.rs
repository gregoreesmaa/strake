//! Permission manifest + enforcement for app capabilities (issue #16).
//!
//! Day-1 Electron compatibility means running untrusted app code, so every
//! capability the shim exposes — filesystem, network, shell, clipboard,
//! native addons — must be gated by a manifest the user (or the packager,
//! issue #14) approves. This module is the Phase-0 policy core: a
//! [`PermissionManifest`] of granted scopes plus an [`Enforcer`] that
//! answers allow/deny for each access, with lexical path containment so
//! `..` escapes cannot walk out of a granted directory. Deny-by-default:
//! anything not granted is denied. OS integration (real dialogs, per-origin
//! prompts) and the `node:*` polyfill surface bind next.

use std::collections::HashSet;

/// A granted path scope: `$APPDATA/scores/*` (prefix) or an exact file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathScope(String);

impl PathScope {
    /// A new scope. A trailing `/*` marks a directory subtree; anything
    /// else is an exact path. The scope is lexically cleaned (see
    /// [`clean_path`]) so a scope containing `..` or `//` cannot silently
    /// never match.
    pub fn new(scope: &str) -> Self {
        match scope.strip_suffix("/*") {
            Some(prefix) => {
                let cleaned = clean_path(prefix);
                // `clean_path` maps an empty/all-slash prefix to `/`, which
                // must not gain a doubled slash: the filesystem root stays
                // spelled `/*`.
                if cleaned == "/" {
                    Self(String::from("/*"))
                } else {
                    Self(format!("{cleaned}/*"))
                }
            }
            None => Self(clean_path(scope)),
        }
    }

    /// Whether `path` falls in this scope. The path is lexically cleaned
    /// (see [`clean_path`]) before the prefix comparison, so callers must
    /// not pre-clean: `..` escapes cannot outrun the scope even when the
    /// caller passes a raw path.
    pub fn contains(&self, path: &str) -> bool {
        let cleaned = clean_path(path);
        match self.0.strip_suffix("/*") {
            Some(prefix) => cleaned == prefix || cleaned.starts_with(&format!("{prefix}/")),
            None => cleaned == self.0,
        }
    }
}

/// A granted network scope: exact host or `*.suffix` wildcard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetScope(String);

impl NetScope {
    /// A new scope (`api.example.com` or `*.example.com`). The scope is
    /// lowercased once here because DNS names are case-insensitive.
    pub fn new(scope: &str) -> Self {
        Self(scope.to_ascii_lowercase())
    }

    /// Whether `host` matches. The host is lowercased before comparison
    /// (DNS is case-insensitive), so callers must not pre-normalize. The
    /// wildcard match stays dot-anchored: `*.example.com` matches
    /// `api.example.com` but not `evilexample.com`.
    pub fn contains(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        match self.0.strip_prefix("*.") {
            Some(suffix) => host == suffix || host.ends_with(&format!(".{suffix}")),
            None => host == self.0,
        }
    }
}

/// What an app may do. Everything defaults to denied.
#[derive(Debug, Default, Clone)]
pub struct PermissionManifest {
    /// Readable filesystem scopes.
    pub fs_read: Vec<PathScope>,
    /// Writable filesystem scopes.
    pub fs_write: Vec<PathScope>,
    /// Reachable network scopes.
    pub net: Vec<NetScope>,
    /// `shell.openExternal` and friends (issue #12 surface).
    pub shell_open: bool,
    /// Clipboard read access.
    pub clipboard_read: bool,
    /// Clipboard write access.
    pub clipboard_write: bool,
    /// Loading native (N-API, issue #18) addons.
    pub native_addons: bool,
}

/// Allow/deny answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Granted by the manifest.
    Allow,
    /// Not granted (deny-by-default).
    Deny,
}

impl Decision {
    /// Whether the access proceeds.
    pub fn is_allow(self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// Lexically clean a `/`-separated path: resolve `.`/`..` and duplicate
/// separators without touching the filesystem. This stops lexical `..`
/// escapes only — it does nothing against symlink escapes (a scoped symlink
/// pointing outside the grant still passes, with the OS resolving the open
/// outside it). The future `node:fs` binding (#16) must close that at the
/// access layer: canonicalize the path and re-check it against the scope,
/// or open with `openat2` and `RESOLVE_IN_ROOT`/`RESOLVE_BENEATH`, to avoid
/// the check-to-use (TOCTOU) race.
pub fn clean_path(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    let mut cleaned = parts.join("/");
    if absolute {
        cleaned.insert(0, '/');
    }
    if cleaned.is_empty() {
        cleaned.push('/');
    }
    cleaned
}

/// Policy enforcer over a manifest.
#[derive(Debug, Clone)]
pub struct Enforcer {
    manifest: PermissionManifest,
    /// Denied capability names, for audit UI and tests.
    denied: HashSet<String>,
}

impl Enforcer {
    /// Enforce a manifest.
    pub fn new(manifest: PermissionManifest) -> Self {
        Self {
            manifest,
            denied: HashSet::new(),
        }
    }

    fn decide(&mut self, capability: &str, granted: bool) -> Decision {
        if granted {
            Decision::Allow
        } else {
            self.denied.insert(capability.to_string());
            Decision::Deny
        }
    }

    /// Filesystem read of `path` (cleaned before scope matching).
    pub fn check_fs_read(&mut self, path: &str) -> Decision {
        let cleaned = clean_path(path);
        let granted = self
            .manifest
            .fs_read
            .iter()
            .any(|scope| scope.contains(&cleaned));
        self.decide("fs.read", granted)
    }

    /// Filesystem write of `path` (cleaned before scope matching).
    pub fn check_fs_write(&mut self, path: &str) -> Decision {
        let cleaned = clean_path(path);
        let granted = self
            .manifest
            .fs_write
            .iter()
            .any(|scope| scope.contains(&cleaned));
        self.decide("fs.write", granted)
    }

    /// Network access to `host` (case-insensitive; normalized on match).
    pub fn check_net(&mut self, host: &str) -> Decision {
        let granted = self.manifest.net.iter().any(|scope| scope.contains(host));
        self.decide("net", granted)
    }

    /// Opening an external URL / revealing files.
    pub fn check_shell_open(&mut self) -> Decision {
        let granted = self.manifest.shell_open;
        self.decide("shell.open", granted)
    }

    /// Clipboard read.
    pub fn check_clipboard_read(&mut self) -> Decision {
        let granted = self.manifest.clipboard_read;
        self.decide("clipboard.read", granted)
    }

    /// Clipboard write.
    pub fn check_clipboard_write(&mut self) -> Decision {
        let granted = self.manifest.clipboard_write;
        self.decide("clipboard.write", granted)
    }

    /// Loading a native addon (issue #18 surface).
    pub fn check_native_addons(&mut self) -> Decision {
        let granted = self.manifest.native_addons;
        self.decide("native.addons", granted)
    }

    /// Capabilities denied so far (audit trail).
    pub fn denied(&self) -> Vec<String> {
        let mut denied: Vec<String> = self.denied.iter().cloned().collect();
        denied.sort();
        denied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> PermissionManifest {
        PermissionManifest {
            fs_read: vec![PathScope::new("/data/scores/*")],
            fs_write: vec![PathScope::new("/data/scores/*")],
            net: vec![NetScope::new("*.example.com")],
            shell_open: false,
            clipboard_read: true,
            clipboard_write: true,
            native_addons: false,
        }
    }

    #[test]
    fn granted_scopes_allow() {
        let mut enforcer = Enforcer::new(manifest());
        assert!(enforcer.check_fs_read("/data/scores/a.json").is_allow());
        assert!(enforcer.check_fs_write("/data/scores/a.json").is_allow());
        assert!(enforcer.check_net("api.example.com").is_allow());
        assert!(
            enforcer.check_net("example.com").is_allow(),
            "bare suffix matches"
        );
        assert!(enforcer.check_clipboard_read().is_allow());
        assert!(enforcer.denied().is_empty(), "allows leave no audit trail");
    }

    #[test]
    fn traversal_escapes_are_denied() {
        let mut enforcer = Enforcer::new(manifest());
        assert_eq!(
            enforcer.check_fs_read("/data/scores/../secrets/key"),
            Decision::Deny,
            "cleaned to /data/secrets/key, outside the scope"
        );
        assert_eq!(
            enforcer.check_fs_write("/data/scores/sub/../../etc/passwd"),
            Decision::Deny
        );
        assert_eq!(enforcer.denied(), vec!["fs.read", "fs.write"]);
    }

    #[test]
    fn deny_by_default_with_audit_trail() {
        let mut enforcer = Enforcer::new(manifest());
        assert_eq!(enforcer.check_net("evil.test"), Decision::Deny);
        assert_eq!(enforcer.check_shell_open(), Decision::Deny);
        assert_eq!(enforcer.check_native_addons(), Decision::Deny);
        // Exact paths do not prefix-match siblings.
        assert_eq!(
            enforcer.check_fs_read("/data/scores-backup/a"),
            Decision::Deny
        );
        assert_eq!(
            enforcer.denied(),
            vec!["fs.read", "native.addons", "net", "shell.open"]
        );
    }

    #[test]
    fn contains_cleans_uncleaned_paths_itself() {
        // `contains` must not trust callers to pre-clean: the raw string
        // "/data/scores/../secrets/key" starts with "/data/scores/" but
        // resolves outside the grant.
        let scope = PathScope::new("/data/scores/*");
        assert!(
            !scope.contains("/data/scores/../secrets/key"),
            "dot-dot escape denied even via `contains` directly"
        );
        assert!(
            !scope.contains("/data/scores/sub/../../etc/passwd"),
            "nested escape denied via `contains` directly"
        );
        assert!(
            scope.contains("/data/scores/a.json"),
            "clean hit still allows"
        );
        assert!(
            scope.contains("/data/scores/./a.json"),
            "dot segments clean to a hit"
        );
        // Scopes are normalized at construction: a scope with `..` or `//`
        // matches by its cleaned form instead of silently never matching.
        assert_eq!(
            PathScope::new("/data/scores/*"),
            PathScope::new("/data//x/../scores/*"),
            "scope normalized in `new`"
        );
        // The filesystem-root scope survives normalization (no `//*`).
        let root = PathScope::new("/*");
        assert!(
            root.contains("/anything/at/all"),
            "root scope still matches"
        );
        assert!(root.contains("/"), "root scope matches root itself");
    }

    #[test]
    fn net_matching_is_case_insensitive_but_dot_anchored() {
        let scope = NetScope::new("*.example.com");
        assert!(
            scope.contains("API.EXAMPLE.COM"),
            "DNS is case-insensitive: mixed-case host matches"
        );
        assert!(
            scope.contains("Example.Com"),
            "bare suffix matches mixed-case too"
        );
        assert!(
            !scope.contains("evilexample.com"),
            "no dot anchor: suffix without a label boundary never matches"
        );
        assert!(
            !scope.contains("EVILEXAMPLE.COM"),
            "dot anchor holds regardless of case"
        );
        assert_eq!(
            NetScope::new("*.EXAMPLE.com"),
            NetScope::new("*.example.com"),
            "scope lowercased once in `new`"
        );
        let mut enforcer = Enforcer::new(manifest());
        assert!(
            enforcer.check_net("API.EXAMPLE.COM").is_allow(),
            "enforcer accepts mixed-case hosts under a wildcard grant"
        );
        assert_eq!(enforcer.check_net("evilexample.com"), Decision::Deny);
    }

    #[test]
    fn clean_path_vectors() {
        assert_eq!(clean_path("/a/./b//c"), "/a/b/c");
        assert_eq!(clean_path("/a/b/../../c"), "/c");
        assert_eq!(
            clean_path("../../etc"),
            "etc",
            "relative escapes clamp at root"
        );
        assert_eq!(clean_path(""), "/");
    }
}
