// maverick-sys/src/identity.rs
// Instance identity + discovery helpers for Maverick.
//
// Every Maverick instance gets a name (from `--name`, default "default") and
// advertises itself under a per-user runtime dir:
//   <runtime_dir>/<name>.sock   — Unix control socket (see `control`)
//   <runtime_dir>/<name>.json   — identity ficha (pid, tty, display, …)
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

/// A live or discovered Maverick instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceInfo {
    /// Instance name (from `--name`).
    pub name: String,
    /// OS process id.
    pub pid: u32,
    /// X11 display, e.g. ":0" (may be empty if unknown).
    pub display: String,
    /// Kernel tty device number from /proc/<pid>/stat field 7.
    pub tty_nr: u64,
    /// Path to the running executable.
    pub exe: String,
    /// Unix epoch seconds when the ficha was written.
    pub started_at: u64,
    /// True if the socket answered a connection (i.e. the WM is actually up).
    pub alive: bool,
}

impl InstanceInfo {
    /// Best-effort human label, e.g. `default (:1, tty 0x8800)`.
    pub fn label(&self) -> String {
        let disp = if self.display.is_empty() {
            "?".to_string()
        } else {
            self.display.clone()
        };
        format!("{} [{} tty={:#x} pid={}]", self.name, disp, self.tty_nr, self.pid)
    }
}

/// Per-user runtime directory for Maverick control files.
///
/// Prefers `$XDG_RUNTIME_DIR/maverick` (typically `/run/user/$UID/maverick`),
/// falling back to `/tmp/maverick-$UID` when the env var is unset (e.g. started
/// from a bare `.xinitrc`).
pub fn runtime_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        if !xdg.is_empty() {
            return Path::new(&xdg).join("maverick");
        }
    }
    let uid = unsafe { libc::getuid() }; // getuid is always safe
    PathBuf::from(format!("/tmp/maverick-{}", uid))
}

/// Full path to the control socket for `name`.
pub fn sock_path(name: &str) -> PathBuf {
    runtime_dir().join(format!("{name}.sock"))
}

/// Full path to the identity ficha for `name`.
pub fn meta_path(name: &str) -> PathBuf {
    runtime_dir().join(format!("{name}.json"))
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
        if let Some(pos) = s.find(')') {
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

/// Path to the executable of a foreign process (readlink /proc/<pid>/exe).
pub fn read_proc_exe(pid: u32) -> String {
    let path = format!("/proc/{pid}/exe");
    std::fs::read_link(&path)
        .ok()
        .and_then(|p| p.into_os_string().into_string().ok())
        .unwrap_or_default()
}

/// Serialize `InstanceInfo` to the ficha JSON file.
pub fn write_meta(info: &InstanceInfo) -> io::Result<()> {
    let dir = runtime_dir();
    std::fs::create_dir_all(&dir)?;
    let json = serde_free_json(info)?;
    std::fs::write(meta_path(&info.name), json)?;
    Ok(())
}

/// Remove the ficha and socket for `name` (call on clean shutdown).
pub fn cleanup_meta(name: &str) {
    let _ = std::fs::remove_file(meta_path(name));
    let _ = std::fs::remove_file(sock_path(name));
}

/// Minimal JSON serializer (no serde dependency) — Maverick ships zero extra
/// deps. Escapes the few fields that could contain special chars.
fn serde_free_json(info: &InstanceInfo) -> io::Result<String> {
    let esc = |s: &str| -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                _ => out.push(c),
            }
        }
        out.push('"');
        out
    };
    Ok(format!(
        "{{\"name\":{n},\"pid\":{p},\"display\":{d},\"tty_nr\":{t},\"exe\":{e},\"started_at\":{s},\"alive\":{a}}}",
        n = esc(&info.name),
        p = info.pid,
        d = esc(&info.display),
        t = info.tty_nr,
        e = esc(&info.exe),
        s = info.started_at,
        a = info.alive,
    ))
}

/// Parse our minimal JSON ficha back into `InstanceInfo` (lenient: missing
/// fields default to empty/0). Enough for our own format, not a general parser.
fn parse_meta(json: &str) -> Option<InstanceInfo> {
    let mut info = InstanceInfo {
        name: String::new(),
        pid: 0,
        display: String::new(),
        tty_nr: 0,
        exe: String::new(),
        started_at: 0,
        alive: false,
    };
    // Strip the outer { } so key/value splitting is straightforward.
    let body = json.trim().strip_prefix('{').unwrap_or(json.trim());
    let body = body.strip_suffix('}').unwrap_or(body);
    for pair in body.split(',') {
        let kv: Vec<&str> = pair.splitn(2, ':').collect();
        if kv.len() != 2 {
            continue;
        }
        let key = kv[0].trim().trim_matches('"');
        let val = kv[1].trim();
        match key {
            "name" => info.name = unquote(val),
            "pid" => info.pid = val.parse().unwrap_or(0),
            "display" => info.display = unquote(val),
            "tty_nr" => info.tty_nr = val.parse().unwrap_or(0),
            "exe" => info.exe = unquote(val),
            "started_at" => info.started_at = val.parse().unwrap_or(0),
            "alive" => info.alive = val == "true",
            _ => {}
        }
    }
    if info.name.is_empty() {
        None
    } else {
        Some(info)
    }
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches('"').to_string()
}

/// Build the `InstanceInfo` for the current process under `name`.
pub fn self_info(name: &str) -> InstanceInfo {
    let pid = std::process::id();
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    InstanceInfo {
        name: name.to_string(),
        pid,
        display: current_display(),
        tty_nr: read_proc_tty(pid),
        exe: read_proc_exe(pid),
        started_at: started,
        alive: true,
    }
}

/// Read a ficha json file from disk, if present.
pub fn read_meta(name: &str) -> Option<InstanceInfo> {
    let p = meta_path(name);
    std::fs::read_to_string(p).ok().and_then(|s| parse_meta(&s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_roundtrip() {
        let info = InstanceInfo {
            name: "dev".into(),
            pid: 1234,
            display: ":1".into(),
            tty_nr: 0x8800,
            exe: "/usr/bin/maverick".into(),
            started_at: 1_700_000_000,
            alive: true,
        };
        let json = serde_free_json(&info).unwrap();
        let back = parse_meta(&json).expect("roundtrip");
        assert_eq!(back.name, "dev");
        assert_eq!(back.pid, 1234);
        assert_eq!(back.display, ":1");
        assert_eq!(back.tty_nr, 0x8800);
    }
}
