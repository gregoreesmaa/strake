//! Native `node:fs` sync primitives behind the issue #16 capability model
//! (issue #154).
//!
//! The JS shape (`existsSync`, `readFileSync`, `Stats`, streams, …) lives in
//! `NODE_FS_BOOTSTRAP_JS` (`electron.rs`); this module holds the thin native
//! layer it calls: `__strake_fs_*` functions that gate every path through
//! the host's [`Enforcer`](strake_electron_compat::Enforcer) and perform the
//! real `std::fs` operation with Node-coded errors (`ENOENT`, `EACCES`, …).
//!
//! Sandboxing posture, documented so embedders grant scopes correctly:
//!
//! * Grants are deny-by-default: without an `fs_read`/`fs_write` scope every
//!   operation fails (`EACCES` for throwing calls, `false` for `existsSync`
//!   so denials are not existence oracles).
//! * Every path is resolved against the app root when relative, lexically
//!   checked against the grant, then canonicalized and re-checked, so `..`
//!   escapes and scoped-symlink redirects are denied even when the target
//!   exists. Grant scopes should be built from canonicalized paths (see
//!   `clean_path`): under a symlinked temp dir, an uncanonicalized scope
//!   spelling never matches the canonicalized access.
//! * The check-then-use window is best-effort, not atomic: a hostile app
//!   that can already win filesystem races is outside the Phase-0 threat
//!   model (see the `openat2` note on
//!   [`clean_path`](strake_electron_compat::clean_path)).

use std::path::{Path, PathBuf};

use boa_engine::object::ObjectInitializer;
use boa_engine::object::builtins::{JsArray, JsArrayBuffer, JsUint8Array};
use boa_engine::property::Attribute;
use boa_engine::{Context, JsError, JsNativeError, JsResult, JsString, JsValue, js_string};

use super::electron::{app_root_dir, electron_state, require_string_arg};

/// Which grant a primitive needs.
#[derive(Clone, Copy)]
pub(crate) enum FsAccess {
    Read,
    Write,
}

/// Outcome of [`scoped_path`] that is not a usable path.
enum ScopedDeny {
    /// Outside the grant (or the grant is absent): callers report `EACCES`.
    Denied,
    /// Inside the grant but missing on disk: callers report `ENOENT`.
    Missing,
}

/// Resolve `raw` to a host path the caller may touch: join relative paths
/// onto the app root, lexically check the grant, then canonicalize and
/// re-check so symlink redirects cannot escape the grant.
fn scoped_path(context: &mut Context, raw: &str, access: FsAccess) -> Result<PathBuf, ScopedDeny> {
    let joined = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        app_root_dir(context).join(raw)
    };
    let shared = electron_state(context).map_err(|_| ScopedDeny::Denied)?;
    let allowed = |path: &str| match access {
        FsAccess::Read => {
            shared.with_permissions(|permissions| permissions.check_fs_read(path).is_allow())
        }
        FsAccess::Write => {
            shared.with_permissions(|permissions| permissions.check_fs_write(path).is_allow())
        }
    };
    if !allowed(&normalize(&joined)) {
        return Err(ScopedDeny::Denied);
    }
    // Canonicalize the nearest existing ancestor (the leaf itself may be a
    // creation target), then re-check the real location. A granted-but-absent
    // leaf stays `Ok`: the operation itself reports `ENOENT`, so reads keep
    // Node's error codes while denials keep `EACCES`.
    let (anchor, rest) = nearest_existing(&joined);
    let canonical = std::fs::canonicalize(&anchor).map_err(|_| ScopedDeny::Missing)?;
    let candidate = rest.map_or(canonical.clone(), |tail| canonical.join(tail));
    if !allowed(&normalize(&candidate)) {
        return Err(ScopedDeny::Denied);
    }
    Ok(candidate)
}

/// Nearest existing ancestor of `path` plus the remaining tail (if any).
/// Components collect in a stack and push onto a fresh tail: joining onto an
/// initially-empty `PathBuf` would leave a trailing separator (`join("")`
/// yields `"leaf/"`), and the OS refuses trailing slashes on files.
fn nearest_existing(path: &Path) -> (PathBuf, Option<PathBuf>) {
    let mut anchor = path.to_path_buf();
    let mut popped: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if anchor.exists() {
            if popped.is_empty() {
                return (anchor, None);
            }
            let mut tail = PathBuf::new();
            for component in popped.iter().rev() {
                tail.push(component);
            }
            return (anchor, Some(tail));
        }
        match anchor.file_name() {
            Some(name) => {
                popped.push(name.to_os_string());
                anchor.pop();
            }
            None => return (anchor, None),
        }
    }
}

/// Scope-matching spelling of a path: `/`-separated (Windows verbatim and
/// separator forms collapse here) and lexically cleaned.
fn normalize(path: &Path) -> String {
    let spelling = path.to_string_lossy();
    #[cfg(windows)]
    let spelling = spelling.replace('\\', "/");
    strake_electron_compat::clean_path(&spelling)
}

/// Node-coded error (`{ code, errno, syscall, path, message }`) built by the
/// `__strake_node_error` factory the fs bootstrap installs; falls back to a
/// plain error when the factory is absent (renderer contexts never install
/// the fs shell, and these primitives are main-only by registration).
fn node_error(
    context: &mut Context,
    code: &str,
    errno: i32,
    syscall: &str,
    path: &str,
    message: String,
) -> JsError {
    let factory = context
        .global_object()
        .get(js_string!("__strake_node_error"), context)
        .ok()
        .and_then(|value| value.as_object());
    if let Some(factory) = factory {
        let args = [
            JsValue::from(JsString::from(code)),
            JsValue::from(errno),
            JsValue::from(JsString::from(syscall)),
            JsValue::from(JsString::from(path)),
            JsValue::from(JsString::from(message.as_str())),
        ];
        if let Ok(thrown) = factory.call(&JsValue::undefined(), &args, context) {
            return JsError::from_opaque(thrown);
        }
    }
    JsError::from(JsNativeError::error().with_message(message))
}

/// Map an I/O failure to its Node `(code, errno)` pair.
fn io_code(error: &std::io::Error) -> (&'static str, i32) {
    use std::io::ErrorKind::{
        AlreadyExists, DirectoryNotEmpty, InvalidInput, IsADirectory, NotADirectory, NotFound,
        PermissionDenied,
    };
    match error.kind() {
        NotFound => ("ENOENT", -2),
        PermissionDenied => ("EACCES", -13),
        AlreadyExists => ("EEXIST", -17),
        IsADirectory => ("EISDIR", -21),
        NotADirectory => ("ENOTDIR", -20),
        DirectoryNotEmpty => ("ENOTEMPTY", -39),
        InvalidInput => ("EINVAL", -22),
        _ => ("EIO", -5),
    }
}

fn io_error(
    context: &mut Context,
    error: &std::io::Error,
    syscall: &'static str,
    path: &str,
) -> JsError {
    let (code, errno) = io_code(error);
    node_error(
        context,
        code,
        errno,
        syscall,
        path,
        format!("{code}: {}, {syscall} '{path}'", error),
    )
}

fn denied_error(context: &mut Context, syscall: &'static str, path: &str) -> JsError {
    node_error(
        context,
        "EACCES",
        -13,
        syscall,
        path,
        format!("EACCES: permission denied, {syscall} '{path}'"),
    )
}

fn deny_to_error(
    context: &mut Context,
    deny: ScopedDeny,
    syscall: &'static str,
    path: &str,
) -> JsError {
    match deny {
        ScopedDeny::Denied => denied_error(context, syscall, path),
        ScopedDeny::Missing => node_error(
            context,
            "ENOENT",
            -2,
            syscall,
            path,
            format!("ENOENT: no such file or directory, {syscall} '{path}'"),
        ),
    }
}

/// Required string path argument (Node names the parameter position, not the
/// value, in its `TypeError`s; these thin primitives keep that shape).
fn fs_path_arg(args: &[JsValue], what: &str, context: &mut Context) -> JsResult<String> {
    let Some(first) = args.first() else {
        return Err(JsError::from(
            JsNativeError::typ().with_message(format!("{what} requires a path argument")),
        ));
    };
    if first.is_null() || first.is_undefined() {
        return Err(JsError::from(
            JsNativeError::typ().with_message(format!("{what} requires a path argument")),
        ));
    }
    crate::dom::to_rust_string(first, context)
}

/// Optional `options.recursive`-style boolean: a bare boolean reads as the
/// flag itself, an options object reads its property, anything else is off.
fn options_flag(args: &[JsValue], index: usize, name: &str, context: &mut Context) -> bool {
    let Some(value) = args.get(index) else {
        return false;
    };
    if let Some(flag) = value.as_boolean() {
        return flag;
    }
    value
        .as_object()
        .and_then(|obj| obj.get(js_string!(name), context).ok())
        .and_then(|flag| flag.as_boolean())
        .unwrap_or(false)
}

/// `__strake_fs_exists(path)`: never throws — denials and misses both read
/// `false`, so callers cannot probe outside their grant.
pub(crate) fn fs_exists(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let raw = fs_path_arg(args, "__strake_fs_exists", context)?;
    let exists = scoped_path(context, &raw, FsAccess::Read)
        .map(|path| path.exists())
        .unwrap_or(false);
    Ok(JsValue::from(exists))
}

/// `__strake_fs_access(path, mode)`: `undefined` on success, coded error on
/// refusal. `W_OK` additionally needs the write grant (Node tests
/// writability, not just visibility).
pub(crate) fn fs_access(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let raw = fs_path_arg(args, "__strake_fs_access", context)?;
    let mode = args.get(1).and_then(JsValue::as_number).unwrap_or(0.0) as u32;
    const W_OK: u32 = 2;
    const X_OK: u32 = 1;
    let path = scoped_path(context, &raw, FsAccess::Read)
        .map_err(|deny| deny_to_error(context, deny, "access", &raw))?;
    if mode & W_OK != 0 {
        let shared = electron_state(context).map_err(|_| denied_error(context, "access", &raw))?;
        let writable = shared.with_permissions(|permissions| {
            permissions.check_fs_write(&normalize(&path)).is_allow()
        });
        if !writable {
            return Err(denied_error(context, "access", &raw));
        }
    }
    let metadata =
        std::fs::metadata(&path).map_err(|error| io_error(context, &error, "access", &raw))?;
    if mode & X_OK != 0 && metadata.permissions().readonly() {
        return Err(denied_error(context, "access", &raw));
    }
    if metadata.permissions().readonly() && mode & W_OK != 0 {
        return Err(denied_error(context, "access", &raw));
    }
    Ok(JsValue::undefined())
}

/// `__strake_fs_mkdir(path, recursive)`.
pub(crate) fn fs_mkdir(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let raw = fs_path_arg(args, "__strake_fs_mkdir", context)?;
    let recursive = options_flag(args, 1, "recursive", context);
    let path = scoped_path(context, &raw, FsAccess::Write)
        .map_err(|_| denied_error(context, "mkdir", &raw))?;
    let result = if recursive {
        std::fs::create_dir_all(&path)
    } else {
        std::fs::create_dir(&path)
    };
    result.map_err(|error| io_error(context, &error, "mkdir", &raw))?;
    Ok(JsValue::undefined())
}

/// `__strake_fs_read(path)`: file bytes as a `Uint8Array`.
pub(crate) fn fs_read(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let raw = fs_path_arg(args, "__strake_fs_read", context)?;
    let path = scoped_path(context, &raw, FsAccess::Read)
        .map_err(|deny| deny_to_error(context, deny, "open", &raw))?;
    let bytes = std::fs::read(&path).map_err(|error| io_error(context, &error, "open", &raw))?;
    Ok(JsValue::from(JsUint8Array::from_iter(bytes, context)?))
}

/// String, `Buffer`/`Uint8Array`, or `ArrayBuffer` write payloads.
fn write_bytes_arg(value: &JsValue, context: &mut Context) -> JsResult<Vec<u8>> {
    if let Some(text) = value.as_string() {
        return Ok(text.to_std_string_escaped().into_bytes());
    }
    if let Some(obj) = value.as_object() {
        if let Ok(view) = JsUint8Array::from_object(obj.clone()) {
            return view.to_vec(context);
        }
        if let Ok(buffer) = JsArrayBuffer::from_object(obj.clone()) {
            return buffer.to_vec().ok_or_else(|| {
                JsError::from(JsNativeError::typ().with_message("ArrayBuffer data is detached"))
            });
        }
    }
    Err(JsError::from(JsNativeError::typ().with_message(
        "data must be a string, Buffer, Uint8Array, or ArrayBuffer",
    )))
}

/// `__strake_fs_write(path, data, append)`.
pub(crate) fn fs_write(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let raw = fs_path_arg(args, "__strake_fs_write", context)?;
    let data = args
        .get(1)
        .map(|value| write_bytes_arg(value, context))
        .unwrap_or_else(|| {
            Err(JsError::from(
                JsNativeError::typ().with_message("__strake_fs_write requires a data argument"),
            ))
        })?;
    let append = args.get(2).and_then(JsValue::as_boolean).unwrap_or(false);
    let path = scoped_path(context, &raw, FsAccess::Write)
        .map_err(|_| denied_error(context, "open", &raw))?;
    if append {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|error| io_error(context, &error, "open", &raw))?;
        file.write_all(&data)
            .map_err(|error| io_error(context, &error, "write", &raw))?;
    } else {
        std::fs::write(&path, &data).map_err(|error| io_error(context, &error, "open", &raw))?;
    }
    Ok(JsValue::undefined())
}

/// `__strake_fs_stat(path, followLinks)`: plain stat data; the JS `Stats`
/// class adds the `isFile()`/`mtime` surface.
pub(crate) fn fs_stat(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let raw = fs_path_arg(args, "__strake_fs_stat", context)?;
    let follow = args.get(1).and_then(JsValue::as_boolean).unwrap_or(true);
    let path = scoped_path(context, &raw, FsAccess::Read)
        .map_err(|deny| deny_to_error(context, deny, "stat", &raw))?;
    let metadata = if follow {
        std::fs::metadata(&path)
    } else {
        std::fs::symlink_metadata(&path)
    }
    .map_err(|error| io_error(context, &error, "stat", &raw))?;
    let millis = |time: std::io::Result<std::time::SystemTime>| {
        time.ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|age| age.as_millis() as f64)
            .unwrap_or(0.0)
    };
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::MetadataExt;
        metadata.mode()
    };
    #[cfg(not(unix))]
    let mode = if metadata.permissions().readonly() {
        0o444
    } else {
        0o666
    };
    let mut init = ObjectInitializer::new(context);
    init.property(js_string!("size"), metadata.len() as f64, Attribute::all());
    init.property(js_string!("mode"), mode, Attribute::all());
    init.property(
        js_string!("mtimeMs"),
        millis(metadata.modified()),
        Attribute::all(),
    );
    init.property(
        js_string!("atimeMs"),
        millis(metadata.accessed()),
        Attribute::all(),
    );
    init.property(
        js_string!("ctimeMs"),
        millis(metadata.created()),
        Attribute::all(),
    );
    init.property(
        js_string!("birthtimeMs"),
        millis(metadata.created()),
        Attribute::all(),
    );
    init.property(js_string!("isFile"), metadata.is_file(), Attribute::all());
    init.property(js_string!("isDir"), metadata.is_dir(), Attribute::all());
    init.property(
        js_string!("isSymlink"),
        metadata.is_symlink(),
        Attribute::all(),
    );
    Ok(JsValue::from(init.build()))
}

/// `__strake_fs_readdir(path)`: entry names (not full paths), sorted for
/// deterministic snapshots.
pub(crate) fn fs_readdir(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let raw = fs_path_arg(args, "__strake_fs_readdir", context)?;
    let path = scoped_path(context, &raw, FsAccess::Read)
        .map_err(|deny| deny_to_error(context, deny, "scandir", &raw))?;
    let mut names: Vec<String> = std::fs::read_dir(&path)
        .map_err(|error| io_error(context, &error, "scandir", &raw))?
        .filter_map(|entry| {
            entry
                .ok()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
        })
        .collect();
    names.sort();
    Ok(JsValue::from(JsArray::from_iter(
        names
            .into_iter()
            .map(|name| JsValue::from(JsString::from(name))),
        context,
    )))
}

/// `__strake_fs_unlink(path)`.
pub(crate) fn fs_unlink(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let raw = fs_path_arg(args, "__strake_fs_unlink", context)?;
    let path = scoped_path(context, &raw, FsAccess::Write)
        .map_err(|_| denied_error(context, "unlink", &raw))?;
    // Reads of a granted-but-absent path report `ENOENT` here (unlike
    // creation-target calls, the target must exist).
    if !path.exists() {
        return Err(deny_to_error(context, ScopedDeny::Missing, "unlink", &raw));
    }
    std::fs::remove_file(&path).map_err(|error| io_error(context, &error, "unlink", &raw))?;
    Ok(JsValue::undefined())
}

/// `__strake_fs_rename(from, to)`.
pub(crate) fn fs_rename(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let from_raw = require_string_arg(args, 0, "__strake_fs_rename")?;
    let to_raw = require_string_arg(args, 1, "__strake_fs_rename")?;
    let from = scoped_path(context, &from_raw, FsAccess::Write)
        .map_err(|_| denied_error(context, "rename", &from_raw))?;
    let to = scoped_path(context, &to_raw, FsAccess::Write)
        .map_err(|_| denied_error(context, "rename", &to_raw))?;
    std::fs::rename(&from, &to).map_err(|error| io_error(context, &error, "rename", &from_raw))?;
    Ok(JsValue::undefined())
}

/// `__strake_fs_copy(from, to)`.
pub(crate) fn fs_copy(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let from_raw = require_string_arg(args, 0, "__strake_fs_copy")?;
    let to_raw = require_string_arg(args, 1, "__strake_fs_copy")?;
    let from = scoped_path(context, &from_raw, FsAccess::Read)
        .map_err(|deny| deny_to_error(context, deny, "copyfile", &from_raw))?;
    let to = scoped_path(context, &to_raw, FsAccess::Write)
        .map_err(|_| denied_error(context, "copyfile", &to_raw))?;
    std::fs::copy(&from, &to).map_err(|error| io_error(context, &error, "copyfile", &from_raw))?;
    Ok(JsValue::undefined())
}

/// `__strake_fs_realpath(path)`: canonical host spelling.
pub(crate) fn fs_realpath(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let raw = fs_path_arg(args, "__strake_fs_realpath", context)?;
    let path = scoped_path(context, &raw, FsAccess::Read)
        .map_err(|deny| deny_to_error(context, deny, "realpath", &raw))?;
    let canonical = std::fs::canonicalize(&path)
        .map_err(|error| io_error(context, &error, "realpath", &raw))?;
    Ok(JsValue::from(JsString::from(
        canonical.to_string_lossy().into_owned(),
    )))
}
