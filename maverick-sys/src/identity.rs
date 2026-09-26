//! Instance identity and discovery helpers for Maverick.
//!
//! Every Maverick instance gets a name (from `--name`, default `"default"`) and
//! advertises itself under a per-user runtime dir, inside a per-session
//! sub-directory named by its random session id (`sid`):
//! ```text
//!   <runtime_dir>/<sid>/control.sock   — Unix control socket (see [`crate::control`])
//!   <runtime_dir>/<sid>/<sid>.json     — identity ficha (pid, tty, display, …)
//! ```
//!
//! This module is what lets an external tool tell three Mavericks on three
//! different TTYs/`DISPLAY`s apart: each ficha records `display` and `tty_nr`,
//! and we can also read `/proc/<pid>` directly as a fallback.
//!
//! # Ownership and lifecycle
//!
//! All paths are derived from [`runtime_dir`] → [`session_dir`] →
//! [`sock_path`]/[`meta_path`]. The per-session directory is created `0700` by
//! [`set_private_dir`]. [`write_meta`] creates the directory and writes the
//! JSON ficha; [`cleanup_meta`] removes both the ficha and the socket on clean
//! shutdown.
//!
//! # `SUN_LEN` invariant
//!
//! Unix sockets are bound as `sockaddr_un.sun_path`, which is 108 bytes on
//! Linux (107 usable + NUL). [`sock_path`] uses a **fixed** filename
//! `control.sock` inside the per-session directory so the random `sid`
//! contributes to the path only once (as the directory name). If the resulting
//! path would exceed `SUN_LEN`, [`try_sock_path`] returns an error rather than
//! silently truncating. See `identity::tests::sock_path_fits_sun_len` for the
//! regression guard.

use std::io;
use std::path::{Path, PathBuf};

/// Default instance name when `--name` is not given.
pub const DEFAULT_NAME: &str = "default";

/// Protocol command strings used over the control socket.
pub const QUIT_CMD: &str = "quit";
pub const PING_CMD: &str = "ping";
pub const IDENTIFY_CMD: &str = "identify";
pub const STATE_CMD: &str = "state";
pub const RESTART_CMD: &str = "restart";
pub const RELOAD_CMD: &str = "reload";
pub const SUBSCRIBE_CMD: &str = "subscribe";
/// `dispatch <action>` — prefix; the remainder is the action name.
pub const DISPATCH_CMD: &str = "dispatch";
/// `query <topic>` — asks the WM to answer a structured JSON query.
pub const QUERY_CMD: &str = "query";

/// A live or discovered Maverick instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceInfo {
    /// Human-readable instance label (from `--name`, default `"default"`).
    pub name: String,
    /// Stable, unique-per-process session id. This is the filesystem key for the
    /// per-session runtime dir / socket / ficha. It is **random** (generated once
    /// at startup), so two simultaneous sessions never collide — `display`/`tty`/
    /// `pid`/`start_time` are kept separately as identity/liveness metadata, not
    /// as the persistent id.
    pub session_id: String,
    /// OS process id.
    pub pid: u32,
    /// X11 display, e.g. ":0" (may be empty if unknown).
    pub display: String,
    /// Kernel tty device number from `/proc/<pid>/stat` field 7.
    pub tty_nr: u64,
    /// Best-effort X server identity ("Xorg"/"XLibre"/"yserver"/"?").
    pub x_server_identity: String,
    /// Kernel start time (boottime-relative) from `/proc/<pid>/stat` field 22.
    /// Used to tell a recycled PID apart from the process we recorded (liveness).
    pub start_time: u64,
    /// Path to the running executable.
    pub exe: String,
    /// Unix epoch seconds when the ficha was written.
    pub started_at: u64,
    /// True if the socket answered a connection (i.e. the WM is actually up).
    pub alive: bool,
}

impl InstanceInfo {
    /// Best-effort human label, e.g. `default (sid=… tty=0x8800 pid=1234)`.
    pub fn label(&self) -> String {
        let disp = if self.display.is_empty() {
            "?".to_string()
        } else {
            self.display.clone()
        };
        format!(
            "{} [sid={} {} tty={:#x} pid={}]",
            self.name, self.session_id, disp, self.tty_nr, self.pid
        )
    }
}

/// Per-user runtime directory for Maverick control files.
///
/// Always `$XDG_RUNTIME_DIR/maverick` (typically `/run/user/$UID/maverick`),
/// the standard XDG Base Directory location for per-user runtime state. When
/// `XDG_RUNTIME_DIR` is unset we fall back to `/run/user/$UID/maverick` (which
/// the login session normally provides) — **never** `/tmp`, which is purged
/// mid-session and would silently lose the session.
pub fn runtime_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        if !xdg.is_empty() {
            return Path::new(&xdg).join("maverick");
        }
    }
    // No XDG_RUNTIME_DIR: never fall back to /tmp (it's purged). Use the
    // standard /run/user/$UID, which the login session normally provides.
    let uid = unsafe { libc::getuid() }; // getuid is always safe
    PathBuf::from(format!("/run/user/{uid}/maverick"))
}

/// Create the runtime directory and make it private (`0700`).
///
/// Every path Maverick keeps at runtime — control sockets, identity fichas,
/// session records, cookies, logs — lives under this one directory, so its mode
/// is the first line of the security story.
///
/// The mode is set here rather than left to `create_dir_all`, because
/// `create_dir_all` applies the process umask to the directories it creates: a
/// session's own directory was being tightened to `0700` by
/// [`set_private_dir`] while the parent it was created *inside* stayed at
/// whatever the umask said, usually `0755`. That leaks the list of session
/// names to every user on the machine, and a name is the address of a control
/// socket.
pub fn ensure_runtime_dir() -> io::Result<PathBuf> {
    let dir = runtime_dir();
    std::fs::create_dir_all(&dir)?;
    set_private_dir(&dir)?;
    Ok(dir)
}

/// Maximum accepted session-id length. Our own ids are ~30 chars
/// (`{pid:x}-{nanos:x}-{rand:x}`); 64 leaves headroom while bounding
/// `sockaddr_un` length and filesystem use from external input.
pub const MAX_SID_LEN: usize = 64;

/// True if `sid` is safe to use as a single path component:
/// non-empty, bounded length, only `[A-Za-z0-9_-]`.
/// Rejects `/`, `.`, `..`, empty and overlong ids (traversal safe).
pub fn is_valid_sid(sid: &str) -> bool {
    if sid.is_empty() || sid.len() > MAX_SID_LEN {
        return false;
    }
    if sid == "." || sid == ".." {
        return false;
    }
    sid.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Validate `sid`, mapping failures to `io::ErrorKind::InvalidInput`.
fn validate_sid(sid: &str) -> io::Result<()> {
    if is_valid_sid(sid) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid session id: {sid:?}"),
        ))
    }
}

/// Per-session sub-directory: `<runtime_dir>/<sid>/`. Created `0700` so other
/// UIDs cannot interfere with this session's socket/ficha.
///
/// An invalid `sid` yields a `__invalid__` sentinel path; prefer
/// [`try_session_dir`] when `sid` comes from external input (CLI, env,
/// directory listing).
pub fn session_dir(sid: &str) -> PathBuf {
    match try_session_dir(sid) {
        Ok(p) => p,
        Err(_) => runtime_dir().join("__invalid__"),
    }
}

/// Fallible version of [`session_dir`]: rejects traversal/overlong ids.
pub fn try_session_dir(sid: &str) -> io::Result<PathBuf> {
    validate_sid(sid)?;
    Ok(runtime_dir().join(sid))
}

/// Full path to the control socket for session `sid`.
///
/// The socket lives inside the per-session directory (`session_dir`) under a
/// FIXED filename `control.sock`, so the random `sid` contributes to the path
/// only ONCE (as the directory name) instead of twice (directory *and*
/// `<sid>.sock`). Spending the `sid` twice against `sockaddr_un.sun_path`'s 107
/// usable bytes is what pushes the path past the budget where `bind`/`connect`
/// fail with a length error.
pub fn sock_path(sid: &str) -> PathBuf {
    match try_sock_path(sid) {
        Ok(p) => p,
        // Never panic on external input; return a sentinel that will
        // fail at bind/connect with a clear OS error instead.
        Err(_) => runtime_dir().join("__invalid__").join("control.sock"),
    }
}

/// Fallible version of [`sock_path`]: validates `sid` and enforces `SUN_LEN`.
pub fn try_sock_path(sid: &str) -> io::Result<PathBuf> {
    let path = try_session_dir(sid)?.join("control.sock");
    // Fail loudly (never silently truncate) if we ever exceed the kernel limit.
    if path.as_os_str().len() >= 108 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("control socket path exceeds SUN_LEN: {path:?}"),
        ));
    }
    Ok(path)
}

/// Full path to the identity ficha for session `sid`.
pub fn meta_path(sid: &str) -> PathBuf {
    match try_meta_path(sid) {
        Ok(p) => p,
        Err(_) => runtime_dir().join("__invalid__.json"),
    }
}

/// Fallible version of [`meta_path`]: rejects traversal via `sid`.
/// The ficha filename embeds `sid`, so validation is mandatory —
/// otherwise `sid = "../../x"` would escape the runtime dir.
pub fn try_meta_path(sid: &str) -> io::Result<PathBuf> {
    validate_sid(sid)?;
    Ok(try_session_dir(sid)?.join(format!("{sid}.json")))
}

/// Generate a fresh, unique-per-process session id.
///
/// The id is random (not derived from display/tty/pid) so two sessions never
/// collide on the filesystem, and it stays stable for the life of the session
/// (generated once at startup, written into the ficha). `display`/`tty`/`pid`/
/// `start_time` are stored separately as identity/liveness metadata.
pub fn new_session_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    // 8 bytes of entropy from /dev/urandom when available; otherwise derive from
    // the clock so we still get a non-trivial, per-boot-varying suffix.
    let rand = read_urandom_u64().unwrap_or(nanos as u64);
    format!("{pid:x}-{nanos:x}-{rand:x}")
}

/// Read 8 bytes of entropy from `/dev/urandom`. `None` when the source is
/// unavailable, so the caller can fall back to the clock.
fn read_urandom_u64() -> Option<u64> {
    use std::io::Read;
    let mut f = std::fs::File::open("/dev/urandom").ok()?;
    let mut buf = [0u8; 8];
    f.read_exact(&mut buf).ok()?;
    Some(u64::from_ne_bytes(buf))
}

/// chmod a path to `0700` so other UIDs cannot read/modify it.
/// Refuses to follow symlinks: if `path` is a symlink (or not a dir),
/// returns an error instead of chmodding an attacker-controlled target.
pub(crate) fn set_private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to chmod non-dir/symlink: {path:?}"),
        ));
    }
    let perm = std::fs::Permissions::from_mode(0o700);
    std::fs::set_permissions(path, perm)
}

/// Current X11 display from the `DISPLAY` env var (best-effort).
pub fn current_display() -> String {
    std::env::var("DISPLAY").unwrap_or_default()
}

/// Kernel tty device number of the current process, read from
/// `/proc/self/stat` field 7. Returns 0 if it cannot be determined
/// (e.g. process has no controlling terminal, as after `setsid`).
pub fn current_tty_nr() -> u64 {
    read_proc_tty(std::process::id())
}

/// Read the X11 `DISPLAY` of a foreign process from `/proc/<pid>/environ`.
///
/// Only works for processes owned by the same uid (the normal case for
/// multiple Mavericks started by one user). Returns "" on any failure.
pub fn read_proc_environ_display(pid: u32) -> String {
    let path = format!("/proc/{pid}/environ");
    match std::fs::read(&path) {
        Ok(bytes) => {
            // /proc/<pid>/environ is a NUL-separated list of KEY=VALUE.
            for kv in bytes.split(|&b| b == 0) {
                if kv.starts_with(b"DISPLAY=") {
                    if let Ok(s) = std::str::from_utf8(&kv[8..]) {
                        return s.to_string();
                    }
                }
            }
            String::new()
        }
        Err(_) => String::new(),
    }
}

/// Read the kernel tty device number of a foreign process from
/// `/proc/<pid>/stat` field 7. Returns 0 if unavailable.
pub fn read_proc_tty(pid: u32) -> u64 {
    let path = format!("/proc/{pid}/stat");
    if let Ok(s) = std::fs::read_to_string(&path) {
        // Format: pid (comm) state ppid pgrp session tty_nr ...
        // `comm` (field 2) may itself contain spaces and parentheses, so the
        // fixed-offset fields are anchored at the last `)` — the remainder of
        // `stat` never contains one.
        if let Some(pos) = s.rfind(')') {
            let rest = &s[pos + 1..];
            let mut fields = rest.split_whitespace();
            // skip state, ppid, pgrp, session
            let _ = fields.next(); // state
            let _ = fields.next(); // ppid
            let _ = fields.next(); // pgrp
            let _ = fields.next(); // session
            if let Some(tty) = fields.next() {
                return tty.parse::<u64>().unwrap_or(0);
            }
        }
    }
    0
}

/// Read the kernel start time (boottime-relative, field 22) of a foreign process
/// from `/proc/<pid>/stat`. Used to distinguish a recycled PID from the process
/// we recorded. Returns 0 if unavailable.
pub fn read_proc_starttime(pid: u32) -> u64 {
    let path = format!("/proc/{pid}/stat");
    if let Ok(s) = std::fs::read_to_string(&path) {
        // After ')': state ppid pgrp session tty_nr tpgid flags minflt cminflt
        // majflt cmajflt utime stime cutime cstime priority nice num_threads
        // itrealvalue <starttime=field 22>
        // Fields are counted from the last `)`: `comm` may contain `)`.
        if let Some(pos) = s.rfind(')') {
            let rest = &s[pos + 1..];
            let mut fields = rest.split_whitespace();
            // skip state, ppid, pgrp, session, tty_nr (5) then tpgid..itrealvalue (14)
            for _ in 0..19 {
                let _ = fields.next();
            }
            if let Some(t) = fields.next() {
                return t.parse::<u64>().unwrap_or(0);
            }
        }
    }
    0
}

/// Path to the executable of a foreign process (readlink `/proc/<pid>/exe`).
pub fn read_proc_exe(pid: u32) -> String {
    let path = format!("/proc/{pid}/exe");
    std::fs::read_link(&path)
        .ok()
        .and_then(|p| p.into_os_string().into_string().ok())
        .unwrap_or_default()
}

/// Serialize `InstanceInfo` to the ficha JSON file.
pub fn write_meta(info: &InstanceInfo) -> io::Result<()> {
    validate_sid(&info.session_id)?;
    ensure_runtime_dir()?;
    let dir = try_session_dir(&info.session_id)?;
    std::fs::create_dir_all(&dir)?;
    set_private_dir(&dir)?;
    let json = serde_free_json(info)?;
    std::fs::write(try_meta_path(&info.session_id)?, json)?;
    Ok(())
}

/// Remove the ficha and socket for session `sid` (call on clean shutdown).
/// Validates `sid` first (no deletion on invalid input) and only unlinks
/// the socket if it really is a socket (via `symlink_metadata`, which does
/// NOT follow symlinks) and the ficha if it is a regular file — never
/// blindly `remove_file`.
pub fn cleanup_meta(sid: &str) {
    if validate_sid(sid).is_err() {
        return;
    }
    let Ok(meta_p) = try_meta_path(sid) else {
        return;
    };
    // Only remove regular files, never symlinks/dirs.
    if let Ok(m) = std::fs::symlink_metadata(&meta_p) {
        if m.file_type().is_file() {
            let _ = std::fs::remove_file(&meta_p);
        }
    }
    let Ok(sock_p) = try_sock_path(sid) else {
        return;
    };
    if let Ok(m) = std::fs::symlink_metadata(&sock_p) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            if m.file_type().is_socket() {
                let _ = std::fs::remove_file(&sock_p);
            }
        }
        #[cfg(not(unix))]
        {
            let _ = m;
        }
    }
}

/// Minimal JSON serializer (no serde dependency) — Maverick ships zero extra
/// deps. Escapes the few fields that could contain special chars.
fn serde_free_json(info: &InstanceInfo) -> io::Result<String> {
    use crate::json::json_quote;
    Ok(format!(
        "{{\"name\":{n},\"session_id\":{s},\"pid\":{p},\"display\":{d},\"tty_nr\":{t},\"x_server_identity\":{x},\"start_time\":{st},\"exe\":{e},\"started_at\":{sa},\"alive\":{a}}}",
        n = json_quote(&info.name),
        s = json_quote(&info.session_id),
        p = info.pid,
        d = json_quote(&info.display),
        t = info.tty_nr,
        x = json_quote(&info.x_server_identity),
        st = info.start_time,
        e = json_quote(&info.exe),
        sa = info.started_at,
        a = info.alive,
    ))
}

/// Parse our minimal JSON ficha back into `InstanceInfo` (lenient: missing
/// fields default to empty/0). Enough for our own format, not a general parser.
/// String values are unescaped, so a field the writer had to escape (a quote, a
/// backslash, a control byte) comes back as it was written. Never panics: the
/// shared [`crate::json::scan_object`] cursor only ever yields `str::get`
/// slices (which return `None` on a non-char-boundary), so a hostile ficha
/// with multibyte UTF-8 cannot DoS us.
fn parse_meta(json: &str) -> Option<InstanceInfo> {
    let mut info = InstanceInfo {
        name: String::new(),
        session_id: String::new(),
        pid: 0,
        display: String::new(),
        tty_nr: 0,
        x_server_identity: String::new(),
        start_time: 0,
        exe: String::new(),
        started_at: 0,
        alive: false,
    };
    for field in crate::json::scan_object(json) {
        // A field the writer emits as a string is read as text whether or not
        // it arrived quoted: a hand-written or half-written ficha that left a
        // number unquoted is still that field's value, not a reason to drop it.
        match field.key {
            "name" => info.name = field.text(),
            "session_id" => info.session_id = field.text(),
            "pid" => info.pid = field.as_u64().unwrap_or(0) as u32,
            "display" => info.display = field.text(),
            "tty_nr" => info.tty_nr = field.as_u64().unwrap_or(0),
            "x_server_identity" => info.x_server_identity = field.text(),
            "start_time" => info.start_time = field.as_u64().unwrap_or(0),
            "exe" => info.exe = field.text(),
            "started_at" => info.started_at = field.as_u64().unwrap_or(0),
            "alive" => info.alive = field.as_bool().unwrap_or(false),
            _ => {}
        }
    }
    if info.session_id.is_empty() || !is_valid_sid(&info.session_id) {
        None
    } else {
        Some(info)
    }
}

/// Build the `InstanceInfo` for the current process under `name` (human label).
/// Allocates a fresh random `session_id` and records liveness metadata.
pub fn self_info(name: &str) -> InstanceInfo {
    // A freshly generated id passes the validation by construction (it is
    // built from the same charset the predicate accepts), so this cannot fail.
    self_info_with_sid(name, &new_session_id()).expect("a generated session id is always valid")
}

/// Build the `InstanceInfo` for the current process under an explicit
/// `session_id`.
///
/// This is what lets a session manager address a window manager by name: the
/// session id is the per-session directory, the control socket path and the
/// ficha filename, so a fixed id means `$XDG_RUNTIME_DIR/maverick/debug/` holds
/// the control socket of the instance the user calls "debug" — the same paths
/// [`discover`] already scans, and the same `sock_path` every tool already
/// builds. Nothing else about the record changes.
///
/// The id is validated by the same predicate that guards every path built from
/// it, and a rejection is an error rather than a silent fallback to a random
/// id: an instance that quietly got a different id would create a second,
/// unreachable runtime directory and advertise itself under a name no tool
/// could find.
pub fn self_info_with_sid(name: &str, sid: &str) -> io::Result<InstanceInfo> {
    validate_sid(sid)?;
    let mut info = self_info_base(name);
    info.session_id = sid.to_string();
    Ok(info)
}

/// Everything [`self_info`] records except the session id, which its two
/// callers choose differently.
fn self_info_base(name: &str) -> InstanceInfo {
    let pid = std::process::id();
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let tty_nr = read_proc_tty(pid);
    InstanceInfo {
        name: name.to_string(),
        session_id: new_session_id(),
        pid,
        display: current_display(),
        tty_nr,
        x_server_identity: x_server_identity(),
        start_time: read_proc_starttime(pid),
        exe: read_proc_exe(pid),
        started_at: started,
        alive: true,
    }
}

/// X server identity placeholder.
///
/// The server binary is not discoverable from the client's `DISPLAY` without
/// extra probing, so this always yields `"?"`; the field exists so the ficha
/// schema (and the `list` column) stays stable.
fn x_server_identity() -> String {
    // Record "?" so the field round-trips through the ficha and `list` renders.
    "?".to_string()
}

/// Read a ficha json file from disk, if present.
/// Returns `None` for invalid `sid` (traversal-safe: no filesystem access).
pub fn read_meta(sid: &str) -> Option<InstanceInfo> {
    let p = try_meta_path(sid).ok()?;
    std::fs::read_to_string(p).ok().and_then(|s| parse_meta(&s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn meta_roundtrip() {
        let info = InstanceInfo {
            name: "dev".into(),
            session_id: "abc123".into(),
            pid: 1234,
            display: ":1".into(),
            tty_nr: 0x8800,
            x_server_identity: "?".into(),
            start_time: 99_999,
            exe: "/usr/bin/maverick".into(),
            started_at: 1_700_000_000,
            alive: true,
        };
        let json = serde_free_json(&info).unwrap();
        let back = parse_meta(&json).expect("roundtrip");
        assert_eq!(back.name, "dev");
        assert_eq!(back.session_id, "abc123");
        assert_eq!(back.pid, 1234);
        assert_eq!(back.display, ":1");
        assert_eq!(back.tty_nr, 0x8800);
        assert_eq!(back.start_time, 99_999);
    }

    #[test]
    fn session_id_is_unique() {
        let a = new_session_id();
        let b = new_session_id();
        assert!(!a.is_empty());
        assert_ne!(a, b);
    }

    #[test]
    fn session_dirs_are_isolated_per_sid() {
        // Two distinct session ids must map to distinct directories, sockets
        // and fichas — two sessions both named `default` must not clobber
        // each other's control socket.
        let a = "aaaaaaaa";
        let b = "bbbbbbbb";
        assert_ne!(session_dir(a), session_dir(b));
        assert_ne!(sock_path(a), sock_path(b));
        assert_ne!(meta_path(a), meta_path(b));
        // A sid must not be treated as empty (parse_meta rejects empty sid).
        assert!(!a.is_empty() && !b.is_empty());
    }

    #[test]
    fn sock_path_fits_sun_len() {
        // Longest realistic sid (pid up to 8 hex + '-' + nanos up to 16 hex +
        // '-' + 16 hex) must keep the socket path under the 108-byte kernel
        // limit (107 usable).
        let long_sid = format!("{:x}-{:x}-{:x}", u32::MAX, u128::MAX, u64::MAX);
        let p = sock_path(&long_sid);
        let len = p.as_os_str().len();
        assert!(
            len < 108,
            "sock_path too long for sockaddr_un: {len} bytes ({p:?})"
        );
        // Fixed filename: the sid appears only as the directory component.
        assert!(
            p.ends_with("control.sock"),
            "socket must use fixed name: {p:?}"
        );
        assert_eq!(p, session_dir(&long_sid).join("control.sock"));
    }

    #[test]
    fn sock_path_is_stable_and_isolated() {
        let s = new_session_id();
        assert_eq!(sock_path(&s), sock_path(&s));
        assert_ne!(sock_path("aaaaaaaa"), sock_path("bbbbbbbb"));
    }

    #[test]
    fn start_time_reads_self() {
        let st = read_proc_starttime(std::process::id());
        assert!(st != 0);
    }

    /// The runtime directory holds every control socket, cookie and log, so its
    /// mode is the boundary. `create_dir_all` would apply the umask instead.
    #[test]
    fn the_runtime_directory_is_private() {
        let dir = ensure_runtime_dir().expect("runtime dir");
        let mode = std::fs::metadata(&dir)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "{} must be owner-only", dir.display());
    }

    #[test]
    fn runtime_dir_never_tmp() {
        let dir = runtime_dir();
        assert!(
            !dir.starts_with("/tmp"),
            "runtime_dir must not be /tmp: {dir:?}"
        );
    }

    /// The whole point of `--session-id`: the paths a tool builds for an
    /// instance are the instance's *name*, so `maverickctl` and the session
    /// manager address the same socket and the same ficha.
    #[test]
    fn an_explicit_sid_names_every_path_the_instance_publishes() {
        let info = self_info_with_sid("debug", "debug").expect("a valid sid");
        assert_eq!(info.session_id, "debug");
        assert_eq!(info.name, "debug");
        // The socket and the ficha the WM will create land under the *name*.
        assert!(
            try_sock_path(&info.session_id)
                .expect("socket path")
                .ends_with("debug/control.sock"),
            "the control socket must be addressed by name"
        );
        assert!(
            try_meta_path(&info.session_id)
                .expect("meta path")
                .ends_with("debug/debug.json"),
            "the identity ficha must be addressed by name"
        );
        // A random id still works, and is still not a name.
        assert!(!self_info("dev").session_id.is_empty());
    }

    /// An id that cannot be a path component must be refused outright. A
    /// silent fallback to a random id would put the socket and the ficha
    /// somewhere no tool computes a path to.
    #[test]
    fn an_unsafe_sid_is_refused_rather_than_replaced() {
        for bad in ["", ".", "..", "../escape", "a/b", "a b", &"x".repeat(65)] {
            assert!(
                self_info_with_sid("dev", bad).is_err(),
                "'{bad}' must not be accepted as a session id"
            );
        }
    }
}

/// Properties of the ficha format, over the two functions that are not part of
/// the public surface: the writer and the reader that discovery trusts.
#[cfg(test)]
mod ficha_props {
    use super::*;
    use crate::prop_support::{config, text};
    use proptest::prelude::*;

    /// A byte-level disturbance, the kind a half-written file, a stale version
    /// left behind by an older build, or a hostile writer produces.
    #[derive(Debug, Clone)]
    enum Edit {
        Insert(usize, char),
        Delete(usize),
        Replace(usize, char),
        Truncate(usize),
    }

    fn edit() -> impl Strategy<Value = Edit> {
        let at = 0usize..24;
        prop_oneof![
            2 => (at.clone(), any::<char>()).prop_map(|(i, c)| Edit::Insert(i, c)),
            1 => at.clone().prop_map(Edit::Delete),
            2 => (at.clone(), any::<char>()).prop_map(|(i, c)| Edit::Replace(i, c)),
            1 => at.prop_map(Edit::Truncate),
        ]
    }

    /// Apply one disturbance in place. Positions are taken modulo the current
    /// length so the strategy can be plain integers; the edit operates on
    /// characters, so it can never split a multi-byte one.
    fn apply_edit(doc: &mut String, e: &Edit) {
        let mut chars: Vec<char> = doc.chars().collect();
        if chars.is_empty() {
            if let Edit::Insert(_, c) = e {
                chars.insert(0, *c);
            }
            *doc = chars.into_iter().collect();
            return;
        }
        let len = chars.len();
        match e {
            Edit::Insert(i, c) => chars.insert(i % len, *c),
            Edit::Delete(i) => {
                chars.remove(i % len);
            }
            Edit::Replace(i, c) => chars[i % len] = *c,
            Edit::Truncate(i) => chars.truncate(i % len),
        }
        *doc = chars.into_iter().collect();
    }

    /// A well-formed ficha: what the WM actually writes.
    fn written_ficha() -> impl Strategy<Value = String> {
        (
            "[A-Za-z0-9_-]{1,64}",
            text(),
            text(),
            text(),
            text(),
            any::<u32>(),
            any::<u64>(),
            any::<u64>(),
            any::<u64>(),
            any::<bool>(),
        )
            .prop_map(
                |(
                    session_id,
                    name,
                    display,
                    x_server_identity,
                    exe,
                    pid,
                    tty_nr,
                    start_time,
                    started_at,
                    alive,
                )| {
                    serde_free_json(&InstanceInfo {
                        name,
                        session_id,
                        pid,
                        display,
                        tty_nr,
                        x_server_identity,
                        start_time,
                        exe,
                        started_at,
                        alive,
                    })
                    .expect("serialization does not fail")
                },
            )
    }

    /// Documents a hostile or half-written ficha can look like: a real one, a
    /// prefix of one (cut mid-string or mid-escape), free-form soup, and any of
    /// those with a few characters edited out from under it.
    fn hostile_ficha() -> impl Strategy<Value = String> {
        (
            prop_oneof![2 => written_ficha(), 1 => text()],
            0usize..48,
            proptest::collection::vec(edit(), 0..4),
        )
            .prop_map(|(mut doc, cut, edits)| {
                doc = doc
                    .char_indices()
                    .nth(cut)
                    .map_or_else(String::new, |(i, _)| doc[..i].to_string());
                for e in edits {
                    apply_edit(&mut doc, &e);
                }
                doc
            })
    }

    // A discovered instance is believed on the strength of its ficha alone, so
    // every field the WM wrote has to come back exactly: a name or executable
    // path holding a quote, a backslash, a comma or a control byte is exactly
    // where a naive reader would truncate the value or re-frame the object.
    proptest! {
        #![proptest_config(config())]
        #[test]
        fn a_written_ficha_reads_back_identically(
            session_id in "[A-Za-z0-9_-]{1,64}",
            name in text(),
            display in text(),
            x_server_identity in text(),
            exe in text(),
            pid in any::<u32>(),
            tty_nr in any::<u64>(),
            start_time in any::<u64>(),
            started_at in any::<u64>(),
            alive in any::<bool>(),
        ) {
            let info = InstanceInfo {
                name,
                session_id,
                pid,
                display,
                tty_nr,
                x_server_identity,
                start_time,
                exe,
                started_at,
                alive,
            };
            let json = serde_free_json(&info).expect("serialization does not fail");
            let back = parse_meta(&json).expect("the WM's own ficha must parse back");
            prop_assert_eq!(back, info);
        }
    }

    // The reader is fed whatever sits in the runtime dir, which is not only the
    // WM's own output. It must always terminate, never slice a multi-byte
    // character and never hand back an entry whose session id discovery would
    // then use to build paths.
    proptest! {
        #![proptest_config(config())]
        #[test]
        fn reading_a_hostile_ficha_never_panics_and_never_invents_a_session(doc in hostile_ficha()) {
            if let Some(info) = parse_meta(&doc) {
                prop_assert!(
                    is_valid_sid(&info.session_id),
                    "ficha yielded a session id that must not be trusted: {:?} from {:?}",
                    info.session_id,
                    doc
                );
            }
        }
    }

    /// A ficha carrying only the string fields an escape test varies; the
    /// numeric and boolean fields are pinned because the reader is not being
    /// exercised on them.
    fn ficha(session_id: &str, name: &str, display: &str, exe: &str) -> InstanceInfo {
        InstanceInfo {
            name: name.to_string(),
            session_id: session_id.to_string(),
            pid: 1234,
            display: display.to_string(),
            tty_nr: 0x8800,
            x_server_identity: "?".to_string(),
            start_time: 99_999,
            exe: exe.to_string(),
            started_at: 1_700_000_000,
            alive: true,
        }
    }

    /// The escape forms are a small fixed set, so they are pinned here rather
    /// than left to the property above: random text rarely lands on the shapes
    /// that break a hand-rolled reader, and each of these asserts the exact
    /// bytes on the wire so a test cannot pass for the wrong reason (a writer
    /// that stopped escaping would satisfy the round trip either way).
    #[test]
    fn an_escaped_quote_survives_the_roundtrip() {
        let info = ficha("sid-quote", "\"quoted\"", ":0", "/usr/bin/maverick");
        let json = serde_free_json(&info).expect("serialization does not fail");
        assert!(
            json.contains(r#""name":"\"quoted\"""#),
            "writer must escape the quotes: {json}"
        );
        assert_eq!(parse_meta(&json).expect("own ficha parses"), info);
    }

    #[test]
    fn an_escaped_backslash_survives_the_roundtrip() {
        let info = ficha("sid-backslash", "a\\b\\c", ":0", "/opt/bin\\maverick");
        let json = serde_free_json(&info).expect("serialization does not fail");
        assert!(
            json.contains(r#""name":"a\\b\\c""#),
            "writer must escape the backslashes: {json}"
        );
        let back = parse_meta(&json).expect("own ficha parses");
        assert_eq!(back.name, "a\\b\\c");
        assert_eq!(back.exe, "/opt/bin\\maverick");
    }

    /// A backslash as the last character of a value puts the writer's `\\`
    /// immediately before the closing quote, the position where a reader that
    /// strips one pair of quotes mistakes the pair for a delimiter.
    #[test]
    fn a_value_ending_in_a_backslash_survives_the_roundtrip() {
        let info = ficha("sid-trailing", "\\", ":0\\", "/usr/bin/maverick");
        let json = serde_free_json(&info).expect("serialization does not fail");
        assert!(
            json.contains(r#""display":":0\\""#),
            "writer must escape the trailing backslash: {json}"
        );
        let back = parse_meta(&json).expect("own ficha parses");
        assert_eq!(back.name, "\\");
        assert_eq!(back.display, ":0\\");
    }

    /// `display` is what tells two instances on different X displays apart, so
    /// a value made only of whitespace has to survive as itself.
    #[test]
    fn a_whitespace_only_value_survives_the_roundtrip() {
        let info = ficha("sid-blank", " ", " ", " ");
        let json = serde_free_json(&info).expect("serialization does not fail");
        assert!(
            json.contains(r#""display":" ""#),
            "writer must not elide the value: {json}"
        );
        let back = parse_meta(&json).expect("own ficha parses");
        assert_eq!(back.name, " ");
        assert_eq!(back.display, " ");
        assert_eq!(back.exe, " ");
    }

    /// A record from an older or slightly different build need not carry every
    /// field; the reader stays lenient and fills the gaps instead of rejecting
    /// the instance or inventing a value for it.
    #[test]
    fn an_absent_field_defaults_instead_of_failing() {
        let doc = r#"{"session_id":"abc123","pid":7,"alive":true}"#;
        let info = parse_meta(doc).expect("a partial ficha is still a ficha");
        assert_eq!(info.session_id, "abc123");
        assert_eq!(info.pid, 7);
        assert!(info.alive);
        assert_eq!(info.name, "");
        assert_eq!(info.display, "");
        assert_eq!(info.exe, "");
        assert_eq!(info.tty_nr, 0);
        assert_eq!(info.start_time, 0);
        assert_eq!(info.started_at, 0);
    }
}
