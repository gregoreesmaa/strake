//! `strake-pack`: minimal canary packager (issue #14, MVP first step).
//!
//! The issue's MVP amendment orders the work: a minimal `strake pack` for
//! canary distribution (bundled dir + version manifest) first, OS
//! signing/notarization and delta auto-updates after. This binary is that
//! first step: it validates an Electron app directory (the same
//! `package.json` contract [`strake-run`](https://github.com/gregoreesmaa/strake/blob/main/apps/run/src/main.rs)
//! boots), copies it into a distributable bundle directory, hashes the
//! bundle with a self-contained SHA-256 (no new external crates), and
//! writes a `latest.json` the updater client
//! ([`UpdateManifest`](strake_electron_compat::UpdateManifest)) parses
//! back — the pack → manifest → select loop stays fully testable without
//! Apple/Windows signing infrastructure.
//!
//! OS installers (`.app`/`.exe`/`.apk` layout, `notarytool`/`signtool`,
//! `bsdiff` deltas) bind the bundle directory and manifest in follow-ups.
//!
//! Exit status is 0 only when the pack validates, copies, hashes, and
//! writes the manifest; the summary prints either way.

use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use strake_electron_compat::{UpdateManifest, Version};

/// What packing an app directory produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackReport {
    /// `package.json` `name` (falls back to the directory name).
    pub app_name: String,
    /// Packed version.
    pub version: String,
    /// Bundle directory (`<out>/bundle/<name>/`).
    pub bundle_dir: PathBuf,
    /// Regular files copied.
    pub file_count: usize,
    /// Total payload bytes copied.
    pub total_bytes: u64,
    /// Hex SHA-256 over the canonical bundle listing (see [`sha256_hex`]).
    pub sha256: String,
    /// Manifest path (`<out>/latest.json`).
    pub manifest_path: PathBuf,
}

/// Why an app directory refused to pack.
#[derive(Debug)]
pub enum PackError {
    /// No `package.json` in the directory (or it could not be read).
    MissingPackageJson { dir: PathBuf },
    /// `package.json` is not valid JSON or not an object.
    InvalidPackageJson { path: PathBuf, message: String },
    /// `package.json` has no usable `version` field.
    MissingVersion { path: PathBuf },
    /// The `version` field does not parse as `major.minor.patch`.
    BadVersion { path: PathBuf, version: String },
    /// The `main` entry file does not exist under the app dir.
    MissingMainEntry { path: PathBuf },
    /// A filesystem step failed.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for PackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingPackageJson { dir } => {
                write!(formatter, "no package.json in {}", dir.display())
            }
            Self::InvalidPackageJson { path, message } => {
                write!(
                    formatter,
                    "invalid package.json at {}: {message}",
                    path.display()
                )
            }
            Self::MissingVersion { path } => {
                write!(formatter, "package.json has no version: {}", path.display())
            }
            Self::BadVersion { path, version } => {
                write!(
                    formatter,
                    "invalid version '{version}' in {}",
                    path.display()
                )
            }
            Self::MissingMainEntry { path } => {
                write!(formatter, "main entry missing: {}", path.display())
            }
            Self::Io { path, source } => {
                write!(formatter, "cannot access {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for PackError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

fn io_error(path: &Path, source: std::io::Error) -> PackError {
    PackError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// SHA-256 over `data`, lowercase hex. Self-contained (std only) so the
/// packager adds no new external crates; verified against the NIST vectors
/// in the tests below.
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity(data.len() + 73);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    let mut block = [0u32; 16];
    let mut schedule = [0u32; 64];
    let (blocks, _) = padded.as_chunks::<64>();
    for chunk in blocks {
        for (word, bytes) in block.iter_mut().zip(chunk.as_chunks::<4>().0) {
            *word = u32::from_be_bytes(*bytes);
        }
        schedule[..16].copy_from_slice(&block);
        for i in 16..64 {
            let word2 = schedule[i - 2];
            let small1 = word2.rotate_right(17) ^ word2.rotate_right(19) ^ (word2 >> 10);
            let word15 = schedule[i - 15];
            let small0 = word15.rotate_right(7) ^ word15.rotate_right(18) ^ (word15 >> 3);
            schedule[i] = schedule[i - 16]
                .wrapping_add(small0)
                .wrapping_add(schedule[i - 7])
                .wrapping_add(small1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h) = (
            state[0], state[1], state[2], state[3], state[4], state[5], state[6], state[7],
        );
        for i in 0..64 {
            let big1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(big1)
                .wrapping_add(choice)
                .wrapping_add(K[i])
                .wrapping_add(schedule[i]);
            let big0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = big0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }
    let mut hex = String::with_capacity(64);
    for word in state {
        hex.push_str(&format!("{word:08x}"));
    }
    hex
}

/// Electron platform name for the running host (matches the updater's
/// `Artifact.platform` vocabulary).
fn host_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "windows",
        "linux" => "linux",
        other => other,
    }
}

/// Electron arch name for the running host.
fn host_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    }
}

fn read_package_json(app_dir: &Path) -> Result<Value, PackError> {
    let path = app_dir.join("package.json");
    let text = std::fs::read_to_string(&path).map_err(|source| {
        if path.is_file() {
            io_error(&path, source)
        } else {
            PackError::MissingPackageJson {
                dir: app_dir.to_path_buf(),
            }
        }
    })?;
    let value: Value =
        serde_json::from_str(&text).map_err(|error| PackError::InvalidPackageJson {
            path: path.clone(),
            message: error.to_string(),
        })?;
    if !value.is_object() {
        return Err(PackError::InvalidPackageJson {
            path,
            message: String::from("top level must be an object"),
        });
    }
    Ok(value)
}

/// Copy `source` into `dest`, skipping `.git` subtrees. Returns the
/// copied regular files as slash-separated paths relative to `dest`,
/// sorted for a deterministic digest.
fn copy_tree(
    source: &Path,
    dest: &Path,
    relative: &str,
    files: &mut Vec<String>,
) -> Result<(), PackError> {
    let read = std::fs::read_dir(source).map_err(|error| io_error(source, error))?;
    let mut names: Vec<_> = read
        .map(|entry| entry.map_err(|error| io_error(source, error)))
        .collect::<Result<_, _>>()?;
    names.sort_by_key(|entry| entry.file_name());
    for entry in names {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == ".git" {
            continue;
        }
        let child = if relative.is_empty() {
            name.to_string()
        } else {
            format!("{relative}/{name}")
        };
        let target = dest.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|error| io_error(&entry.path(), error))?;
        if file_type.is_dir() {
            std::fs::create_dir_all(&target).map_err(|error| io_error(&target, error))?;
            copy_tree(&entry.path(), &target, &child, files)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &target).map_err(|error| io_error(&target, error))?;
            files.push(child);
        }
    }
    Ok(())
}

/// Pack `app_dir` into `out_dir`: `<out>/bundle/<name>/` plus
/// `<out>/latest.json` (see the module docs).
pub fn pack_app(app_dir: &Path, out_dir: &Path, channel: &str) -> Result<PackReport, PackError> {
    let manifest = read_package_json(app_dir)?;
    let package_path = app_dir.join("package.json");
    let app_name = manifest
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            app_dir
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| String::from("app"))
        });
    let version_text = manifest
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| PackError::MissingVersion {
            path: package_path.clone(),
        })?;
    let version = Version::parse(version_text).ok_or_else(|| PackError::BadVersion {
        path: package_path,
        version: version_text.to_string(),
    })?;
    let main_entry = manifest
        .get("main")
        .and_then(Value::as_str)
        .unwrap_or("index.js");
    let main_path = app_dir.join(main_entry);
    if !main_path.is_file() {
        return Err(PackError::MissingMainEntry { path: main_path });
    }

    let bundle_dir = out_dir.join("bundle").join(&app_name);
    std::fs::create_dir_all(&bundle_dir).map_err(|error| io_error(&bundle_dir, error))?;
    let mut files = Vec::new();
    copy_tree(app_dir, &bundle_dir, "", &mut files)?;

    // Canonical bundle digest: sorted `path NUL bytes NUL` records, so the
    // manifest pins exactly what ships (the updater re-verifies this form).
    let mut canonical = Vec::new();
    let mut total_bytes = 0u64;
    for relative in &files {
        let bytes = std::fs::read(bundle_dir.join(relative))
            .map_err(|error| io_error(&bundle_dir, error))?;
        total_bytes += bytes.len() as u64;
        canonical.extend_from_slice(relative.as_bytes());
        canonical.push(0);
        canonical.extend_from_slice(&bytes);
        canonical.push(0);
    }
    let sha256 = sha256_hex(&canonical);

    let manifest_path = out_dir.join("latest.json");
    let document = json!({
        "version": version_text,
        "channel": channel,
        "files": [{
            "platform": host_platform(),
            "arch": host_arch(),
            "url": format!("bundle/{app_name}"),
            "sha256": sha256,
            "kind": "bundle-dir",
        }],
    });
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&document).expect("manifest serializes"),
    )
    .map_err(|error| io_error(&manifest_path, error))?;

    // The manifest must feed the updater client it ships alongside: a pack
    // whose `latest.json` does not parse (or does not select for this
    // host) is a failed pack, not a publishable one.
    let parsed = UpdateManifest::parse(
        &std::fs::read_to_string(&manifest_path)
            .map_err(|error| io_error(&manifest_path, error))?,
    )
    .expect("packed manifest parses");
    debug_assert!(parsed.version.compare(&version) == std::cmp::Ordering::Equal);
    debug_assert!(
        parsed
            .select(
                &Version::parse("0.0.0").expect("zero parses"),
                host_platform(),
                host_arch()
            )
            .is_some()
    );

    Ok(PackReport {
        app_name,
        version: version_text.to_string(),
        bundle_dir,
        file_count: files.len(),
        total_bytes,
        sha256,
        manifest_path,
    })
}

fn usage() -> ! {
    eprintln!("usage: strake-pack [--channel NAME] <app-dir> --out <dist-dir>");
    std::process::exit(2);
}

fn main() {
    // Index-based parsing (`--out`/`--channel` consume the next argument):
    // a `for` loop cannot advance the iterator mid-body, and
    // `while let ... = args.next()` trips `clippy::while_let_on_iterator`.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut channel = String::from("stable");
    let mut dir: Option<String> = None;
    let mut out: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--out" {
            i += 1;
            out = args.get(i).cloned();
        } else if arg == "--channel" {
            i += 1;
            channel = args
                .get(i)
                .cloned()
                .unwrap_or_else(|| String::from("stable"));
        } else if dir.is_none() {
            dir = Some(arg.clone());
        } else {
            usage();
        }
        i += 1;
    }
    let (Some(dir), Some(out)) = (dir, out) else {
        usage()
    };
    match pack_app(Path::new(&dir), Path::new(&out), &channel) {
        Ok(report) => {
            println!("app: {} {}", report.app_name, report.version);
            println!(
                "bundle: {} ({} files, {} bytes)",
                report.bundle_dir.display(),
                report.file_count,
                report.total_bytes
            );
            println!("sha256: {}", report.sha256);
            println!("manifest: {}", report.manifest_path.display());
        }
        Err(error) => {
            eprintln!("strake-pack: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_nist_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    fn scratch(suite: &str) -> PathBuf {
        std::env::temp_dir().join(format!("strake-pack-test-{}-{suite}", std::process::id()))
    }

    fn fixture_app(root: &Path) {
        let app = root.join("demo-app");
        std::fs::create_dir_all(app.join("pages")).expect("fixture dirs");
        std::fs::write(
            app.join("package.json"),
            r#"{"name": "demo-app", "version": "1.2.3", "main": "main.js"}"#,
        )
        .expect("fixture package.json");
        std::fs::write(app.join("main.js"), "const { app } = require('electron');").expect("main");
        std::fs::write(app.join("pages/index.html"), "<title>demo</title>").expect("page");
        std::fs::create_dir_all(app.join(".git")).expect("git dir");
        std::fs::write(app.join(".git/HEAD"), "ref: refs/heads/main").expect("git file");
    }

    #[test]
    fn pack_round_trips_through_updater_manifest() {
        let root = scratch("round-trip");
        let _ = std::fs::remove_dir_all(&root);
        fixture_app(&root);
        let out = root.join("dist");
        let report = pack_app(&root.join("demo-app"), &out, "beta").expect("packs");
        assert_eq!(report.app_name, "demo-app");
        assert_eq!(report.version, "1.2.3");
        assert_eq!(report.file_count, 3, "package.json + main.js + page");
        assert!(report.bundle_dir.join("main.js").is_file());
        assert!(report.bundle_dir.join("pages/index.html").is_file());
        assert!(!report.bundle_dir.join(".git").exists(), ".git stays out");
        assert_eq!(report.sha256.len(), 64);

        let text = std::fs::read_to_string(&report.manifest_path).expect("manifest written");
        let manifest = UpdateManifest::parse(&text).expect("updater parses pack output");
        assert_eq!(manifest.channel, "beta");
        let current = Version::parse("1.0.0").expect("current parses");
        assert!(manifest.has_update_for(&current));
        let artifact = manifest
            .select(&current, host_platform(), host_arch())
            .expect("host artifact selects");
        assert_eq!(artifact.sha256, report.sha256);
        assert_eq!(artifact.url, "bundle/demo-app");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pack_rejects_unversioned_app() {
        let root = scratch("unversioned");
        let _ = std::fs::remove_dir_all(&root);
        let app = root.join("noversion");
        std::fs::create_dir_all(&app).expect("dirs");
        std::fs::write(app.join("package.json"), r#"{"name": "x", "main": "m.js"}"#)
            .expect("package.json");
        std::fs::write(app.join("m.js"), "").expect("main");
        let error = pack_app(&app, &root.join("dist"), "stable").expect_err("needs a version");
        assert!(matches!(error, PackError::MissingVersion { .. }), "{error}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pack_rejects_missing_main_entry() {
        let root = scratch("no-main");
        let _ = std::fs::remove_dir_all(&root);
        let app = root.join("nomain");
        std::fs::create_dir_all(&app).expect("dirs");
        std::fs::write(
            app.join("package.json"),
            r#"{"name": "x", "version": "0.1.0", "main": "gone.js"}"#,
        )
        .expect("package.json");
        let error = pack_app(&app, &root.join("dist"), "stable").expect_err("needs main");
        assert!(
            matches!(error, PackError::MissingMainEntry { .. }),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
