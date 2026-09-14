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

/// One open file descriptor: virtual fds (allocated from 100 up, so they
/// never collide with stdio) map to host files here. Rights are fixed at
/// `open` — later reads/writes trust the table, POSIX-style — so the
/// capability check happens once, on the canonical path, at open time.
pub(crate) struct OpenFd {
    file: std::fs::File,
    append: bool,
}

/// First virtual fd: clearly outside stdio (0/1/2), small enough to read
/// naturally in logs and snapshots.
pub(crate) const FIRST_FD: u32 = 100;

impl OpenFd {
    pub(crate) fn new(file: std::fs::File, append: bool) -> Self {
        Self { file, append }
    }

    pub(crate) fn file_mut(&mut self) -> &mut std::fs::File {
        &mut self.file
    }

    pub(crate) fn appends(&self) -> bool {
        self.append
    }
}

/// Parsed `open`/`openSync` flags: string (`'r'`, `'w+'`, …) or numeric
/// `O_*` bitmask. Only the access/creation subset is modeled; exotic bits
/// (`O_SYNC`, `O_DIRECT`, …) are accepted and ignored — all host IO is
/// synchronous and unbuffered passthrough anyway.
struct OpenFlags {
    read: bool,
    write: bool,
    create: bool,
    exclusive: bool,
    truncate: bool,
    append: bool,
}

#[cfg(target_os = "linux")]
mod open_bits {
    pub(crate) const CREAT: u32 = 64;
    pub(crate) const EXCL: u32 = 128;
    pub(crate) const TRUNC: u32 = 512;
    pub(crate) const APPEND: u32 = 1024;
}

#[cfg(target_os = "macos")]
mod open_bits {
    pub(crate) const CREAT: u32 = 512;
    pub(crate) const EXCL: u32 = 2048;
    pub(crate) const TRUNC: u32 = 1024;
    pub(crate) const APPEND: u32 = 8;
}

#[cfg(target_os = "windows")]
mod open_bits {
    pub(crate) const CREAT: u32 = 256;
    pub(crate) const EXCL: u32 = 1024;
    pub(crate) const TRUNC: u32 = 512;
    pub(crate) const APPEND: u32 = 8;
}

// Other platforms: access-mode bits (0/1/2) are universal; creation bits
// degrade to create-without-exclusive. These must match the JS `constants`
// table in `NODE_STANDIN_BOOTSTRAP_JS`.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod open_bits {
    pub(crate) const CREAT: u32 = 0;
    pub(crate) const EXCL: u32 = 0;
    pub(crate) const TRUNC: u32 = 0;
    pub(crate) const APPEND: u32 = 0;
}

fn parse_open_flags(value: &JsValue) -> JsResult<OpenFlags> {
    if let Some(number) = value.as_number() {
        // `as u32` saturates negatives to 0 (`O_RDONLY`): Node rejects
        // those with `EINVAL`, so check the float first.
        if number < 0.0 {
            return Err(JsError::from(
                JsNativeError::typ().with_message("invalid open flags"),
            ));
        }
        let bits = number as u32;
        return Ok(OpenFlags {
            read: bits & 0b11 != 1,
            write: bits & 0b11 != 0,
            create: bits & open_bits::CREAT != 0,
            exclusive: bits & open_bits::EXCL != 0,
            truncate: bits & open_bits::TRUNC != 0,
            append: bits & open_bits::APPEND != 0,
        });
    }
    if let Some(flag) = value.as_string() {
        let spelled = flag.to_std_string_escaped();
        let parsed = match spelled.as_str() {
            "r" => (true, false, false, false, false, false),
            "r+" | "rs" | "rs+" => (true, true, false, false, false, false),
            "w" => (false, true, true, false, true, false),
            "wx" => (false, true, true, true, false, false),
            "w+" => (true, true, true, false, true, false),
            "wx+" => (true, true, true, true, false, false),
            "a" => (false, true, true, false, false, true),
            "ax" => (false, true, true, true, false, true),
            "a+" => (true, true, true, false, false, true),
            "ax+" => (true, true, true, true, false, true),
            _ => {
                return Err(JsError::from(
                    JsNativeError::typ()
                        .with_message(format!("unknown file open flag '{spelled}'")),
                ));
            }
        };
        return Ok(OpenFlags {
            read: parsed.0,
            write: parsed.1,
            create: parsed.2,
            exclusive: parsed.3,
            truncate: parsed.4,
            append: parsed.5,
        });
    }
    Err(JsError::from(
        JsNativeError::typ().with_message("flags must be a string or number"),
    ))
}

fn ebadf_error(context: &mut Context, syscall: &'static str) -> JsError {
    node_error(
        context,
        "EBADF",
        -9,
        syscall,
        "",
        format!("EBADF: bad file descriptor, {syscall}"),
    )
}

fn fd_arg(args: &[JsValue], what: &str) -> JsResult<u32> {
    args.first()
        .and_then(JsValue::as_number)
        .map(|fd| fd as u32)
        .ok_or_else(|| {
            JsError::from(JsNativeError::typ().with_message(format!("{what} requires an fd")))
        })
}

/// `__strake_fs_open(path, flags, mode)`: capability-checked once, on the
/// canonical path, per direction (read grant for readers, write grant for
/// writers, both for `O_RDWR`); later fd ops trust the table, POSIX-style.
pub(crate) fn fs_open(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let raw = fs_path_arg(args, "__strake_fs_open", context)?;
    let flags = args
        .get(1)
        .map(parse_open_flags)
        .transpose()?
        .unwrap_or(OpenFlags {
            read: true,
            write: false,
            create: false,
            exclusive: false,
            truncate: false,
            append: false,
        });
    let mode = args
        .get(2)
        .and_then(JsValue::as_number)
        .unwrap_or(0o666 as f64) as u32;
    let joined = if Path::new(&raw).is_absolute() {
        PathBuf::from(&raw)
    } else {
        app_root_dir(context).join(&raw)
    };
    let (anchor, rest) = nearest_existing(&joined);
    let canonical = std::fs::canonicalize(&anchor)
        .map_err(|_| deny_to_error(context, ScopedDeny::Missing, "open", &raw))?;
    let candidate = rest.map_or(canonical.clone(), |tail| canonical.join(tail));
    let shared = electron_state(context).map_err(|_| denied_error(context, "open", &raw))?;
    let spelling = normalize(&candidate);
    if flags.read
        && !shared.with_permissions(|permissions| permissions.check_fs_read(&spelling).is_allow())
    {
        return Err(denied_error(context, "open", &raw));
    }
    if (flags.write || flags.append)
        && !shared.with_permissions(|permissions| permissions.check_fs_write(&spelling).is_allow())
    {
        return Err(denied_error(context, "open", &raw));
    }
    let mut options = std::fs::OpenOptions::new();
    options
        .read(flags.read)
        .write(flags.write || flags.append)
        .append(flags.append)
        .create(flags.create || flags.exclusive)
        .create_new(flags.exclusive)
        .truncate(flags.truncate && !flags.append);
    #[cfg(unix)]
    if flags.create || flags.exclusive {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let file = options
        .open(&candidate)
        .map_err(|error| io_error(context, &error, "open", &raw))?;
    Ok(JsValue::from(
        shared.fs_fd_open(OpenFd::new(file, flags.append)),
    ))
}

/// `__strake_fs_close(fd)`: unknown fds read `EBADF`, as in Node.
pub(crate) fn fs_close(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let fd = fd_arg(args, "__strake_fs_close")?;
    let shared = electron_state(context).map_err(|_| ebadf_error(context, "close"))?;
    if !shared.fs_fd_close(fd) {
        return Err(ebadf_error(context, "close"));
    }
    Ok(JsValue::undefined())
}

/// Shared range validation for fd reads/writes: `offset`/`length` default
/// to the whole tail and must stay inside the byte source.
fn fd_range(total: usize, offset: Option<f64>, length: Option<f64>) -> JsResult<(usize, usize)> {
    let total_f = total as f64;
    let offset_f = offset.unwrap_or(0.0).max(0.0);
    let length_f = length.unwrap_or(total_f - offset_f).max(0.0);
    if !(0.0..=total_f).contains(&offset_f) || !(0.0..=total_f - offset_f).contains(&length_f) {
        return Err(JsError::from(
            JsNativeError::range().with_message("offset/length out of range"),
        ));
    }
    Ok((offset_f as usize, length_f as usize))
}

fn position_arg(args: &[JsValue], index: usize) -> Option<u64> {
    args.get(index)
        .and_then(JsValue::as_number)
        .filter(|position| *position >= 0.0)
        .map(|position| position as u64)
}

/// `__strake_fs_read_fd(fd, buffer, offset, length, position)`: bytes read
/// into the caller's `Uint8Array`/`Buffer`. A numeric `position` reads
/// there without moving the cursor (pread semantics); null reads at the
/// cursor and advances it.
pub(crate) fn fs_read_fd(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let fd = fd_arg(args, "__strake_fs_read_fd")?;
    let view = args
        .get(1)
        .and_then(JsValue::as_object)
        .and_then(|obj| JsUint8Array::from_object(obj).ok())
        .ok_or_else(|| {
            JsError::from(
                JsNativeError::typ().with_message("buffer must be a Uint8Array or Buffer"),
            )
        })?;
    let view_len = view.length(context)?;
    let (offset, length) = fd_range(
        view_len,
        args.get(2).and_then(JsValue::as_number),
        args.get(3).and_then(JsValue::as_number),
    )?;
    let position = position_arg(args, 4);
    let shared = electron_state(context).map_err(|_| ebadf_error(context, "read"))?;
    let outcome = shared.fs_fd_with(fd, |handle| {
        use std::io::{Read as _, Seek as _, SeekFrom};
        let file = handle.file_mut();
        // Save/restore around positioned reads so the cursor never moves.
        let saved = position.map(|_| file.stream_position());
        if let Some(at) = position {
            file.seek(SeekFrom::Start(at))?;
        }
        let mut chunk = vec![0u8; length];
        let mut read = 0usize;
        while read < length {
            match file.read(&mut chunk[read..]) {
                Ok(0) => break,
                Ok(n) => read += n,
                Err(error) => {
                    if let Ok(Some(cursor)) = saved.transpose() {
                        let _ = file.seek(SeekFrom::Start(cursor));
                    }
                    return Err(error);
                }
            }
        }
        if let Ok(Some(cursor)) = saved.transpose() {
            file.seek(SeekFrom::Start(cursor))?;
        }
        Ok((read, chunk))
    });
    let (read, chunk) = match outcome {
        None => return Err(ebadf_error(context, "read")),
        Some(Err(error)) => return Err(io_error(context, &error, "read", "")),
        Some(Ok(done)) => done,
    };
    if read > 0 {
        let staged = JsUint8Array::from_iter(chunk.into_iter().take(read), context)?;
        view.set_values(JsValue::from(staged), Some(offset as u64), context)?;
    }
    Ok(JsValue::from(read as f64))
}

/// `__strake_fs_write_fd(fd, bytes, offset, length, position)`: bytes
/// written, pretty much the write end of [`fs_read_fd`]. Append-mode fds
/// always land at the end, ignoring `position`.
pub(crate) fn fs_write_fd(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let fd = fd_arg(args, "__strake_fs_write_fd")?;
    let data = args
        .get(1)
        .map(|value| write_bytes_arg(value, context))
        .unwrap_or_else(|| {
            Err(JsError::from(
                JsNativeError::typ().with_message("__strake_fs_write_fd requires data"),
            ))
        })?;
    let (offset, length) = fd_range(
        data.len(),
        args.get(2).and_then(JsValue::as_number),
        args.get(3).and_then(JsValue::as_number),
    )?;
    let position = position_arg(args, 4);
    let shared = electron_state(context).map_err(|_| ebadf_error(context, "write"))?;
    let outcome = shared.fs_fd_with(fd, |handle| {
        use std::io::{Seek as _, SeekFrom, Write as _};
        let appends = handle.appends();
        let file = handle.file_mut();
        if appends {
            file.seek(SeekFrom::End(0))?;
        } else if let Some(at) = position {
            let saved = file.stream_position().ok();
            file.seek(SeekFrom::Start(at))?;
            let result = file.write_all(&data[offset..offset + length]);
            if let Some(cursor) = saved {
                let _ = file.seek(SeekFrom::Start(cursor));
            }
            return result.map(|()| length);
        }
        file.write_all(&data[offset..offset + length])
            .map(|()| length)
    });
    match outcome {
        None => Err(ebadf_error(context, "write")),
        Some(Err(error)) => Err(io_error(context, &error, "write", "")),
        Some(Ok(done)) => Ok(JsValue::from(done as f64)),
    }
}

/// Outcome of [`scoped_path`] that is not a usable path.
enum ScopedDeny {
    /// Outside the grant (or the grant is absent): callers report `EACCES`.
    Denied,
    /// Inside the grant but missing on disk: callers report `ENOENT`.
    Missing,
}

/// Resolve `raw` to a host path the caller may touch: join relative paths
/// onto the app root, resolve symlinks through the nearest existing
/// ancestor, then check the grant against the canonical location — so a
/// `/var/...` spelling reaches a `/private/...` grant, while `..` escapes
/// and scoped-symlink redirects still read `EACCES`. A granted-but-absent
/// leaf stays `Ok`: the operation itself reports `ENOENT`, so reads keep
/// Node's error codes while denials keep `EACCES`.
fn scoped_path(context: &mut Context, raw: &str, access: FsAccess) -> Result<PathBuf, ScopedDeny> {
    let joined = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        app_root_dir(context).join(raw)
    };
    let shared = electron_state(context).map_err(|_| ScopedDeny::Denied)?;
    let allowed = |path: &Path| {
        let spelling = normalize(path);
        match access {
            FsAccess::Read => shared
                .with_permissions(|permissions| permissions.check_fs_read(&spelling).is_allow()),
            FsAccess::Write => shared
                .with_permissions(|permissions| permissions.check_fs_write(&spelling).is_allow()),
        }
    };
    // Canonicalize the nearest existing ancestor (the leaf itself may be a
    // creation target). When nothing canonicalizes — no existing anchor,
    // or a race removed it — fall back to the lexical spelling so
    // out-of-grant paths still read `EACCES` instead of `ENOENT`.
    let (anchor, rest) = nearest_existing(&joined);
    let candidate = match std::fs::canonicalize(&anchor) {
        Ok(canonical) => rest.map_or(canonical.clone(), |tail| canonical.join(tail)),
        Err(_) => {
            return Err(if allowed(&joined) {
                ScopedDeny::Missing
            } else {
                ScopedDeny::Denied
            });
        }
    };
    if !allowed(&candidate) {
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
pub(crate) fn node_error(
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

/// `__strake_fs_readlink(path)`: link target (issue #155 — `graceful-fs`
/// touches `fs.promises.readlink` at import time).
pub(crate) fn fs_readlink(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let raw = fs_path_arg(args, "__strake_fs_readlink", context)?;
    // `readlink` names the link itself: scope the parent (which
    // canonicalizes safely — an escaping parent still reads denied) and keep
    // the leaf lexical, since `scoped_path` would resolve the very link
    // being read. Naming an outside target never opens it.
    let path = match Path::new(&raw).file_name() {
        Some(leaf) => {
            let parent_raw = Path::new(&raw)
                .parent()
                .map(|parent| parent.to_string_lossy().into_owned())
                .unwrap_or_default();
            let parent = scoped_path(context, &parent_raw, FsAccess::Read)
                .map_err(|deny| deny_to_error(context, deny, "readlink", &raw))?;
            parent.join(leaf)
        }
        None => scoped_path(context, &raw, FsAccess::Read)
            .map_err(|deny| deny_to_error(context, deny, "readlink", &raw))?,
    };
    let target =
        std::fs::read_link(&path).map_err(|error| io_error(context, &error, "readlink", &raw))?;
    Ok(JsValue::from(JsString::from(
        target.to_string_lossy().into_owned(),
    )))
}

/// `__strake_fs_utimes(path, atimeSecs, mtimeSecs)` (issue #155): Joplin's
/// lock heartbeat leans on mtime. `filetime` sets both stamps — what `std`
/// cannot do (atime included, no silent half-write).
pub(crate) fn fs_utimes(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    fn secs_arg(args: &[JsValue], index: usize, name: &str) -> JsResult<(i64, u32)> {
        let value = args
            .get(index)
            .and_then(|arg| arg.as_number())
            .ok_or_else(|| {
                JsError::from(
                    JsNativeError::typ()
                        .with_message(format!("The \"{name}\" argument must be of type number")),
                )
            })?;
        let whole = value.floor();
        let mut nanos = ((value - whole) * 1_000_000_000.0).round() as u32;
        let mut secs = whole as i64;
        if nanos >= 1_000_000_000 {
            secs += 1;
            nanos -= 1_000_000_000;
        }
        Ok((secs, nanos))
    }
    let raw = fs_path_arg(args, "__strake_fs_utimes", context)?;
    let (atime_secs, atime_nanos) = secs_arg(args, 1, "atime")?;
    let (mtime_secs, mtime_nanos) = secs_arg(args, 2, "mtime")?;
    let path = scoped_path(context, &raw, FsAccess::Write)
        .map_err(|_| denied_error(context, "utimes", &raw))?;
    filetime::set_file_times(
        &path,
        filetime::FileTime::from_unix_time(atime_secs, atime_nanos),
        filetime::FileTime::from_unix_time(mtime_secs, mtime_nanos),
    )
    .map_err(|error| io_error(context, &error, "utimes", &raw))?;
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
