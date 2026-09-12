//! Delta-update manifest client (issue #14, Phase-0).
//!
//! Issue #14 spans the packager CLI (`strake pack`), the auto-updater, and
//! store installers. This module is the updater's client half — the part
//! that runs inside every shipped app: parse a `latest.json` manifest,
//! decide whether an artifact is newer than the running build, verify the
//! manifest carries a signature envelope, and pick applicable delta
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
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre.to_string())),
            None => (text, None),
        };
        let core = core.split('+').next().unwrap_or(core);
        if core.is_empty() {
            return None;
        }
        let mut release = Vec::new();
        for part in core.split('.') {
            release.push(part.parse::<u64>().ok()?);
        }
        Some(Self { release, pre })
    }

    /// Compare per semver: longer release wins on prefix equality, and a
    /// prerelease sorts below its release.
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
            (Some(left), Some(right)) => left.cmp(right),
        }
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
                sha256: string_field(file, "sha256")?,
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
    /// Returns `None` when this target is not published.
    pub fn select<'manifest>(
        &'manifest self,
        current: &Version,
        platform: &str,
        arch: &str,
    ) -> Option<&'manifest Artifact> {
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
                "sha256": "aa",
                "signature": "sig-full"
            },
            {
                "platform": "darwin",
                "arch": "arm64",
                "url": "https://cdn.example.com/app-1.3.0-1.4.0.delta",
                "sha256": "bb",
                "signature": "sig-delta",
                "deltaFrom": "1.3.0"
            },
            {
                "platform": "linux",
                "arch": "x64",
                "url": "https://cdn.example.com/app-1.4.0.AppImage",
                "sha256": "cc"
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
