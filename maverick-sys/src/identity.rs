// maverick-sys/src/identity.rs
// Instance identity + discovery helpers for Maverick.
//
// Every Maverick instance gets a name (from `--name`, default "default") and
// advertises itself under a per-user runtime dir, inside a per-session
// sub-directory named by its random session id (`sid`):
//   <runtime_dir>/<sid>/control.sock   — Unix control socket (see `control`)
//   <runtime_dir>/<sid>/<sid>.json     — identity ficha (pid, tty, display, …)
//
// This module is what lets an external tool tell three Mavericks on three
// different TTYs/DISPLAYs apart: each ficha records `display` and `tty_nr`,
// and we can also read /proc/<pid> directly as a fallback.

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

/// Per-session sub-directory: `<runtime_dir>/<sid>/`. Created `0700` so other
/// UIDs cannot interfere with this session's socket/ficha.
pub fn session_dir(sid: &str) -> PathBuf {
    runtime_dir().join(sid)
}

/// Full path to the control socket for session `sid`.
///
/// The socket lives inside the per-session directory (`session_dir`) under a
/// FIXED filename `control.sock`. The random `sid` therefore contributes to the
/// path only ONCE (as the directory name), never twice — this keeps the total
/// length well under the `sockaddr_un.sun_path` limit (107 usable bytes on
/// Linux) even for the longest realistic `sid`, and avoids the
/// `path must be shorter than SUN_LEN` failure that occurred when `sid` was
/// embedded both as the directory AND as `<sid>.sock`.
pub fn sock_path(sid: &str) -> PathBuf {
    let path = session_dir(sid).join("control.sock");
    // Fail loudly (never silently truncate) if we ever exceed the kernel limit.
    assert!(
        path.as_os_str().len() < 108,
        "control socket path exceeds SUN_LEN: {path:?}"
    );
    path
}

/// Full path to the identity ficha for session `sid`.
pub fn meta_path(sid: &str) -> PathBuf {
    session_dir(sid).join(format!("{sid}.json"))
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

/// Read 8 bytes of entropy from `/dev/urandom`.
fn read_urandom_u64() -> Option<u64> {
    use std::io::Read;
    let mut f = std::fs::File::open("/dev/urandom").ok()?;
    let mut buf = [0u8; 8];
    f.read_exact(&mut buf).ok()?;
    Some(u64::from_ne_bytes(buf))
}

/// chmod a path to `0700` so other UIDs cannot read/modify it.
pub(crate) fn set_private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
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
        // comm may contain spaces/parens, so find the first ')' and count from there.
        // Use rfind to handle process names containing ')' themselves.
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
    let dir = session_dir(&info.session_id);
    std::fs::create_dir_all(&dir)?;
    set_private_dir(&dir)?;
    let json = serde_free_json(info)?;
    std::fs::write(meta_path(&info.session_id), json)?;
    Ok(())
}

/// Remove the ficha and socket for session `sid` (call on clean shutdown).
pub fn cleanup_meta(sid: &str) {
    let _ = std::fs::remove_file(meta_path(sid));
    let _ = std::fs::remove_file(sock_path(sid));
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
    // Walk the JSON body manually rather than splitting on ',' to correctly
    // handle commas that appear inside quoted string values.
    let body = json.trim().strip_prefix('{').unwrap_or(json.trim());
    let body = body.strip_suffix('}').unwrap_or(body);
    let bytes = body.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // Skip whitespace and commas
        while i < len && (bytes[i] == b' ' || bytes[i] == b'\t' || bytes[i] == b',') {
            i += 1;
        }
        if i >= len {
            break;
        }
        // Read key (unquoted or quoted)
        let key = if bytes[i] == b'"' {
            i += 1;
            let start = i;
            while i < len && bytes[i] != b'"' {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            let k = &body[start..i];
            i += 1;
            k
        } else {
            let start = i;
            while i < len && bytes[i] != b':' && bytes[i] != b' ' {
                i += 1;
            }
            &body[start..i]
        };
        // Skip ':' and whitespace
        while i < len && (bytes[i] == b':' || bytes[i] == b' ') {
            i += 1;
        }
        // Read value: either a quoted string or a bare token until next ',' or '}'
        let val = if i < len && bytes[i] == b'"' {
            i += 1;
            let start = i;
            while i < len {
                if bytes[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'"' {
                    break;
                }
                i += 1;
            }
            let v = &body[start..i];
            if i < len {
                i += 1;
            }
            v
        } else {
            let start = i;
            while i < len && bytes[i] != b',' && bytes[i] != b'}' {
                i += 1;
            }
            body[start..i].trim()
        };
        match key.trim_matches('"') {
            "name" => info.name = unquote(val),
            "session_id" => info.session_id = unquote(val),
            "pid" => info.pid = val.parse().unwrap_or(0),
            "display" => info.display = unquote(val),
            "tty_nr" => info.tty_nr = val.parse().unwrap_or(0),
            "x_server_identity" => info.x_server_identity = unquote(val),
            "start_time" => info.start_time = val.parse().unwrap_or(0),
            "exe" => info.exe = unquote(val),
            "started_at" => info.started_at = val.parse().unwrap_or(0),
            "alive" => info.alive = val == "true",
            _ => {}
        }
    }
    if info.session_id.is_empty() {
        None
    } else {
        Some(info)
    }
}

fn unquote(s: &str) -> String {
    crate::json::json_unescape(s.trim().trim_matches('"'))
}

/// Build the `InstanceInfo` for the current process under `name` (human label).
/// Allocates a fresh random `session_id` and records liveness metadata.
pub fn self_info(name: &str) -> InstanceInfo {
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

/// Best-effort X server identity. Unknown here, but recorded so `list` can show
/// an "XSERVER" column; the value is enriched from `DISPLAY` semantics only.
fn x_server_identity() -> String {
    // The actual server binary is not trivially discoverable without extra
    // probing; record "?" so the field round-trips and the column renders.
    "?".to_string()
}

/// Read a ficha json file from disk, if present.
pub fn read_meta(sid: &str) -> Option<InstanceInfo> {
    let p = meta_path(sid);
    std::fs::read_to_string(p).ok().and_then(|s| parse_meta(&s))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // Two distinct session ids must live in distinct sub-directories with
        // distinct socket/ficha paths — this is the core fix for C1 (two
        // `default` sessions clobbering each other).
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
        // limit (107 usable). Regression for `path must be shorter than SUN_LEN`.
        let long_sid = format!("{:x}-{:x}-{:x}", u32::MAX, u128::MAX, u64::MAX);
        let p = sock_path(&long_sid);
        let len = p.as_os_str().len();
        assert!(
            len < 108,
            "sock_path too long for sockaddr_un: {len} bytes ({p:?})"
        );
        // Fixed filename: the sid appears only as the directory component.
        assert!(p.ends_with("control.sock"), "socket must use fixed name: {p:?}");
        assert_eq!(p, session_dir(&long_sid).join("control.sock"));
    }

    #[test]
    fn sock_path_is_stable_and_isolated() {
        // Same sid -> same path; distinct sids -> distinct paths (isolation).
        let s = new_session_id();
        assert_eq!(sock_path(&s), sock_path(&s));
        assert_ne!(sock_path("aaaaaaaa"), sock_path("bbbbbbbb"));
    }

    #[test]
    fn start_time_reads_self() {
        // Our own start time must be non-zero (the kernel always reports one).
        let st = read_proc_starttime(std::process::id());
        assert!(st != 0);
    }

    #[test]
    fn runtime_dir_never_tmp() {
        let dir = runtime_dir();
        assert!(
            !dir.starts_with("/tmp"),
            "runtime_dir must not be /tmp: {dir:?}"
        );
    }
}
