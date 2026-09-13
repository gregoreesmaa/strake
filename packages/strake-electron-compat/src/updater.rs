//! Delta-update manifest client (issue #14, Phase-0).
//!
//! Issue #14 spans the packager CLI (`strake pack`), the auto-updater, and
//! store installers. This module is the updater's client half — the part
//! that runs inside every shipped app: parse a `latest.json` manifest,
//! decide whether an artifact is newer than the running build, record the
//! per-artifact signature envelope for the download/apply flow to verify,
//! and pick applicable delta
//! artifacts. Bundle generation (`.app`/`.exe`/`.apk` layout, signing,
//! notarization) and the download/apply/restart flow bind next. Version
//! ordering is a minimal semver implementation (no new dependencies).

use serde_json::Value;

/// A parsed `major.minor.patch[-pre][+build]` version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    /// Release components.
    pub release: Vec<u64>,
    /// Prerelease identifiers (`None` for a final release).
    pub pre: Option<String>,
}

impl Version {
    /// Parse `1.2.3`, `1.2.3-beta.1`, or `1.2.3+build` (a leading `v` is
    /// accepted, matching update-feed conventions).
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.strip_prefix('v').unwrap_or(text);
        // Strip `+build` metadata before splitting `-pre`: build metadata
        // may itself contain `-` (semver §10), which must not be mistaken
        // for the prerelease separator.
        let text = text.split('+').next().unwrap_or(text);
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre.to_string())),
            None => (text, None),
        };
        if core.is_empty() {
            return None;
        }
        let mut release = Vec::new();
        for part in core.split('.') {
            release.push(part.parse::<u64>().ok()?);
        }
        Some(Self { release, pre })
    }

    /// Compare per semver: longer release wins on prefix equality, a
    /// prerelease sorts below its release, and prerelease identifiers
    /// compare per semver §11 (numeric identifiers numerically, numeric
    /// below alphanumeric, longer set wins on prefix equality).
    pub fn compare(&self, other: &Self) -> std::cmp::Ordering {
        let width = self.release.len().max(other.release.len());
        for index in 0..width {
            let left = self.release.get(index).copied().unwrap_or(0);
            let right = other.release.get(index).copied().unwrap_or(0);
            match left.cmp(&right) {
                std::cmp::Ordering::Equal => {}
                order => return order,
            }
        }
        match (&self.pre, &other.pre) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some(_), None) => std::cmp::Ordering::Less,
            (Some(left), Some(right)) => compare_prerelease(left, right),
        }
    }
}

/// Compare dot-separated prerelease strings per semver §11: identifiers
/// compare left to right, and a longer set wins on prefix equality.
fn compare_prerelease(left: &str, right: &str) -> std::cmp::Ordering {
    let mut left_ids = left.split('.');
    let mut right_ids = right.split('.');
    loop {
        match (left_ids.next(), right_ids.next()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(left), Some(right)) => match compare_identifier(left, right) {
                std::cmp::Ordering::Equal => {}
                order => return order,
            },
        }
    }
}

/// Compare one prerelease identifier per semver §11.4: identifiers of only
/// ASCII digits compare numerically (by length, then lexically, so there is
/// no overflow limit), numeric identifiers sort below alphanumeric ones,
/// and alphanumeric identifiers compare lexically in ASCII order.
fn compare_identifier(left: &str, right: &str) -> std::cmp::Ordering {
    let left_numeric = !left.is_empty() && left.bytes().all(|byte| byte.is_ascii_digit());
    let right_numeric = !right.is_empty() && right.bytes().all(|byte| byte.is_ascii_digit());
    match (left_numeric, right_numeric) {
        (true, true) => left.len().cmp(&right.len()).then_with(|| left.cmp(right)),
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        (false, false) => left.cmp(right),
    }
}

/// One downloadable artifact in the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    /// Target platform (`darwin`, `windows`, `linux`, `android`).
    pub platform: String,
    /// Target architecture (`x64`, `arm64`, ...).
    pub arch: String,
    /// Download URL.
    pub url: String,
    /// Expected SHA-256 hex digest.
    pub sha256: String,
    /// Signature envelope (Ed25519 over the bytes, per issue #14).
    pub signature: Option<String>,
    /// Full version this delta applies from (`None` for full installers).
    pub delta_from: Option<Version>,
}

/// A parsed `latest.json` update manifest.
#[derive(Debug, Clone)]
pub struct UpdateManifest {
    /// Newest available version.
    pub version: Version,
    /// Release channel (`stable`, `beta`, ...).
    pub channel: String,
    /// Downloadable artifacts.
    pub artifacts: Vec<Artifact>,
}

/// Manifest parse failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    /// Not valid JSON / wrong shape.
    InvalidShape(String),
    /// The `version` field does not parse.
    BadVersion(String),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidShape(message) => write!(f, "invalid update manifest: {message}"),
            Self::BadVersion(version) => write!(f, "invalid update version '{version}'"),
        }
    }
}

impl std::error::Error for ManifestError {}

fn string_field(value: &Value, name: &str) -> Result<String, ManifestError> {
    value
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ManifestError::InvalidShape(format!("missing string field '{name}'")))
}

/// Read a `sha256` field, failing fast when it is not a 64-character
/// lowercase-or-uppercase hex digest. The digest is the trust anchor for
/// the download-verify step, so a truncated or non-hex feed value must not
/// parse into an expected digest.
fn sha256_field(value: &Value) -> Result<String, ManifestError> {
    let digest = string_field(value, "sha256")?;
    let valid = digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit());
    if !valid {
        return Err(ManifestError::InvalidShape(format!(
            "invalid sha256 digest '{digest}'"
        )));
    }
    Ok(digest)
}

impl UpdateManifest {
    /// Parse `latest.json` text.
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let json: Value = serde_json::from_str(text)
            .map_err(|error| ManifestError::InvalidShape(error.to_string()))?;
        let version_text = string_field(&json, "version")?;
        let version =
            Version::parse(&version_text).ok_or(ManifestError::BadVersion(version_text))?;
        let channel = json
            .get("channel")
            .and_then(Value::as_str)
            .unwrap_or("stable")
            .to_string();
        let files = json
            .get("files")
            .and_then(Value::as_array)
            .ok_or_else(|| ManifestError::InvalidShape(String::from("missing 'files' array")))?;
        let mut artifacts = Vec::with_capacity(files.len());
        for file in files {
            let delta_from = match file.get("deltaFrom").and_then(Value::as_str) {
                None => None,
                Some(text) => Some(
                    Version::parse(text)
                        .ok_or_else(|| ManifestError::BadVersion(text.to_string()))?,
                ),
            };
            artifacts.push(Artifact {
                platform: string_field(file, "platform")?,
                arch: string_field(file, "arch")?,
                url: string_field(file, "url")?,
                sha256: sha256_field(file)?,
                signature: file
                    .get("signature")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                delta_from,
            });
        }
        Ok(Self {
            version,
            channel,
            artifacts,
        })
    }

    /// Whether the manifest offers anything newer than `current`.
    pub fn has_update_for(&self, current: &Version) -> bool {
        self.version.compare(current) == std::cmp::Ordering::Greater
    }

    /// Pick the best artifact for a platform/arch: an applicable delta when
    /// the running version matches `delta_from`, else a full installer.
    /// Returns `None` when this target is not published, or when `current`
    /// is already at (or newer than) the manifest version: an updater must
    /// never offer a downgrade, so callers need no separate
    /// [`has_update_for`](Self::has_update_for) check.
    pub fn select<'manifest>(
        &'manifest self,
        current: &Version,
        platform: &str,
        arch: &str,
    ) -> Option<&'manifest Artifact> {
        if !self.has_update_for(current) {
            return None;
        }
        let candidates: Vec<&Artifact> = self
            .artifacts
            .iter()
            .filter(|artifact| artifact.platform == platform && artifact.arch == arch)
            .collect();
        candidates
            .iter()
            .find(|artifact| {
                artifact
                    .delta_from
                    .as_ref()
                    .is_some_and(|from| from.compare(current) == std::cmp::Ordering::Equal)
            })
            .copied()
            .or_else(|| {
                candidates
                    .into_iter()
                    .find(|artifact| artifact.delta_from.is_none())
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"{
        "version": "1.4.0",
        "channel": "stable",
        "files": [
            {
                "platform": "darwin",
                "arch": "arm64",
                "url": "https://cdn.example.com/app-1.4.0-full.zip",
                "sha256": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
                "signature": "sig-full"
            },
            {
                "platform": "darwin",
                "arch": "arm64",
                "url": "https://cdn.example.com/app-1.3.0-1.4.0.delta",
                "sha256": "5e884898da28047151d0e56f8dc6292773603d0d6aabbdd62a11ef721d1542d8",
                "signature": "sig-delta",
                "deltaFrom": "1.3.0"
            },
            {
                "platform": "linux",
                "arch": "x64",
                "url": "https://cdn.example.com/app-1.4.0.AppImage",
                "sha256": "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
            }
        ]
    }"#;

    #[test]
    fn versions_order_per_semver() {
        let release = Version::parse("1.4.0").expect("parse");
        assert!(Version::parse("v1.4.0").expect("v-prefix") == release);
        assert_eq!(
            release.compare(&Version::parse("1.3.9").expect("parse")),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            release.compare(&Version::parse("1.4.0-beta.1").expect("parse")),
            std::cmp::Ordering::Greater,
            "release beats its prerelease"
        );
        assert_eq!(
            Version::parse("1.4").expect("parse").compare(&release),
            std::cmp::Ordering::Equal,
            "shorter release pads with zeros"
        );
        assert_eq!(Version::parse("not-a-version"), None);
    }

    #[test]
    fn build_metadata_with_dash_is_not_a_prerelease() {
        // `+build-1` metadata must not split into a prerelease: a final
        // release with metadata compares equal to itself without metadata.
        let with_meta = Version::parse("1.2.3+build-1").expect("parse");
        let plain = Version::parse("1.2.3").expect("parse");
        assert_eq!(with_meta, plain);
        assert_eq!(
            with_meta.compare(&plain),
            std::cmp::Ordering::Equal,
            "build metadata is ignored in precedence"
        );
        assert_eq!(
            with_meta.compare(&Version::parse("1.2.3-beta").expect("parse")),
            std::cmp::Ordering::Greater,
            "a final release with build metadata still beats its prerelease"
        );
    }

    #[test]
    fn prereleases_compare_per_semver_section_11() {
        let beta2 = Version::parse("1.4.0-beta.2").expect("parse");
        let beta11 = Version::parse("1.4.0-beta.11").expect("parse");
        assert_eq!(
            beta11.compare(&beta2),
            std::cmp::Ordering::Greater,
            "numeric identifiers compare numerically, not lexicographically"
        );
        assert_eq!(
            Version::parse("1.4.0-1").expect("parse").compare(&beta2),
            std::cmp::Ordering::Less,
            "numeric identifiers sort below alphanumeric ones"
        );
        assert_eq!(
            Version::parse("1.4.0-alpha")
                .expect("parse")
                .compare(&Version::parse("1.4.0-alpha.1").expect("parse")),
            std::cmp::Ordering::Less,
            "a longer identifier set wins on prefix equality"
        );
        assert_eq!(
            beta2.compare(&Version::parse("1.4.0-beta.2").expect("parse")),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn manifest_selects_deltas_then_fulls() {
        let manifest = UpdateManifest::parse(MANIFEST).expect("parse");
        assert_eq!(manifest.channel, "stable");
        assert!(manifest.has_update_for(&Version::parse("1.3.0").expect("parse")));
        assert!(!manifest.has_update_for(&Version::parse("1.4.0").expect("parse")));
        assert!(!manifest.has_update_for(&Version::parse("2.0.0").expect("parse")));

        let current = Version::parse("1.3.0").expect("parse");
        let picked = manifest
            .select(&current, "darwin", "arm64")
            .expect("artifact");
        assert!(picked.url.ends_with(".delta"), "applicable delta wins");
        assert_eq!(picked.signature.as_deref(), Some("sig-delta"));

        let older = Version::parse("1.2.0").expect("parse");
        let picked = manifest
            .select(&older, "darwin", "arm64")
            .expect("artifact");
        assert!(
            picked.url.ends_with("-full.zip"),
            "stale clients take the full"
        );
        assert!(picked.delta_from.is_none());

        let linux = manifest.select(&older, "linux", "x64").expect("artifact");
        assert_eq!(
            linux.signature, None,
            "signatures stay optional per artifact"
        );

        assert_eq!(manifest.select(&older, "windows", "x64"), None);

        let newer = Version::parse("2.0.0").expect("parse");
        assert_eq!(
            manifest.select(&newer, "darwin", "arm64"),
            None,
            "no artifact — not even the full installer — is offered as a downgrade"
        );
        let same = Version::parse("1.4.0").expect("parse");
        assert_eq!(
            manifest.select(&same, "darwin", "arm64"),
            None,
            "an up-to-date build is offered nothing"
        );
    }

    fn manifest_with_digest(digest: &str) -> String {
        format!(
            r#"{{
            "version": "1.4.0",
            "channel": "stable",
            "files": [
                {{
                    "platform": "darwin",
                    "arch": "arm64",
                    "url": "https://cdn.example.com/app-1.4.0-full.zip",
                    "sha256": "{digest}"
                }}
            ]
        }}"#
        )
    }

    #[test]
    fn truncated_or_non_hex_digests_fail_at_parse() {
        for digest in [
            "aa",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b85",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b85500",
            "zzb0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ] {
            assert!(
                matches!(
                    UpdateManifest::parse(&manifest_with_digest(digest)),
                    Err(ManifestError::InvalidShape(_))
                ),
                "digest '{digest}' must fail fast at parse time"
            );
        }
        UpdateManifest::parse(&manifest_with_digest(
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ))
        .expect("a 64-hex digest parses");
    }

    #[test]
    fn malformed_manifests_fail_loudly() {
        assert!(matches!(
            UpdateManifest::parse("not json"),
            Err(ManifestError::InvalidShape(_))
        ));
        assert!(matches!(
            UpdateManifest::parse(r#"{"version": "1.0"}"#),
            Err(ManifestError::InvalidShape(_))
        ));
        assert!(matches!(
            UpdateManifest::parse(r#"{"version": "nope", "files": []}"#),
            Err(ManifestError::BadVersion(_))
        ));
    }
}
