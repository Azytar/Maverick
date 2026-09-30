//! Discovery and remote control for Maverick instances.
//!
//! Scans the per-user runtime dir for per-session subdirectories, enriches the
//! identity ficha found in each with live `/proc` data (`DISPLAY`, `tty_nr`,
//! `exe`), and offers operations to quit one or all instances by name. This is
//! what lets a tool tell three Mavericks on three different TTYs/`DISPLAY`s
//! apart and target the right one.
//!
//! # Ownership and lifecycle
//!
//! No owned handles — all functions are stateless and re-scan
//! [`maverick_sys::identity::runtime_dir`] on every call. File I/O is best-effort;
//! a missing or unreadable ficha is silently skipped.
//!
//! # Stale-socket and PID-reuse guard
//!
//! [`list_instances`] marks an entry `alive` only when **both** conditions hold:
//!
//! 1. The control socket answers [`maverick_sys::control::ping`] (proves a live listener).
//! 2. The recorded `pid`'s `/proc/<pid>/stat` start time matches the ficha's
//!    `start_time` (field 22). If the WM crashed and the kernel recycled the
//!    PID, the start time will differ and the entry is considered stale even if
//!    some unrelated process now holds that PID or a dead socket file remains.
//!
//! The socket's own stale file was already handled at spawn by
//! [`maverick_sys::control::ControlServer::spawn`] (TOCTOU-safe `is_socket` check
//! before unlink), but a `SIGKILL`'d instance may still leave a dead socket
//! that rejects connections — the `ping` check catches it.

use crate::client;
use std::fs;

use maverick_sys::identity::{self, InstanceInfo};

/// List every Maverick instance with a ficha on disk.
///
/// Each session lives in its own subdirectory of [`maverick_sys::identity::runtime_dir`]
/// named after its `session_id`, and the ficha is `<sid>/<sid>.json`. Missing
/// `display`/`tty_nr`/`exe` fields are filled in from `/proc/<pid>`, and
/// `alive` comes from the ping + start-time check documented at module level.
pub fn list_instances() -> Vec<InstanceInfo> {
    let dir = identity::runtime_dir();
    let mut out = Vec::new();

    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return out,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        // Each session is its own subdirectory named after the session id.
        // Use `symlink_metadata` (no following): a symlink farm pointing at
        // `/etc` etc. must not be traversed.
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
            continue;
        }
        let sid = match path.file_name().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        // Reject traversal ids (`..`, `a/b`, overlong) before any path is
        // built from them.
        if !identity::is_valid_sid(&sid) {
            continue;
        }
        let mut info = match identity::read_meta(&sid) {
            Some(i) => i,
            None => continue,
        };

        // Fill gaps in the ficha from /proc so TTYs/DISPLAYs can be told apart.
        if info.display.is_empty() {
            info.display = identity::read_proc_environ_display(info.pid);
        }
        if info.tty_nr == 0 {
            info.tty_nr = identity::read_proc_tty(info.pid);
        }
        if info.exe.is_empty() {
            info.exe = identity::read_proc_exe(info.pid);
        }

        info.alive = is_instance_alive(&info);

        out.push(info);
    }

    out.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    out
}

/// True if the instance's socket answers a ping *and* the recorded pid is
/// still the same process. Without the start-time half, a crashed instance
/// whose pid the kernel has since recycled would look alive through its
/// leftover socket file.
fn is_instance_alive(info: &InstanceInfo) -> bool {
    if client::ping(&info.session_id).is_err() {
        return false;
    }
    if info.start_time != 0 {
        let live_start = identity::read_proc_starttime(info.pid);
        if live_start == 0 || live_start != info.start_time {
            return false;
        }
    }
    true
}

/// Find one instance by exact human name or session id.
pub fn find_by_name(name: &str) -> Option<InstanceInfo> {
    list_instances()
        .into_iter()
        .find(|i| i.name == name || i.session_id == name)
}

/// Ask a single instance (by session id) to quit via its control socket.
/// Returns the server reply or an error if it can't be reached.
///
/// This does not clean up after the instance, and that is deliberate. A managed
/// session's X server is owned by the session record, not by the window
/// manager, so unlinking the ficha and socket from here left the X server alive
/// holding its display while the record still read `running` — and it deleted
/// the very two files `wm_is_up` uses to tell a live window manager from a dead
/// one, so the next command reported a running manager as `crashed` and stopped
/// its display server out from under it. Callers acting on a managed session go
/// through [`crate::session::lifecycle::stop`], which orders all of that
/// correctly; this remains the path for an instance nobody has a record of,
/// where the window manager owns its own socket and removes it on exit.
pub fn quit_by_name(sid: &str) -> std::io::Result<String> {
    client::quit(sid)
}

/// Quit every discovered instance that is still alive.
/// Returns a summary of (session_id, result).
pub fn quit_all() -> Vec<(String, std::io::Result<String>)> {
    let live: Vec<String> = list_instances()
        .into_iter()
        .filter(|i| i.alive)
        .map(|i| i.session_id)
        .collect();
    live.into_iter()
        .map(|sid| {
            let r = quit_by_name(&sid);
            (sid, r)
        })
        .collect()
}

/// Remove stale fichas whose socket no longer answers or whose PID is gone.
/// Returns the session ids removed.
pub fn prune_stale() -> Vec<String> {
    let mut removed = Vec::new();
    for info in list_instances() {
        if !info.alive {
            identity::cleanup_meta(&info.session_id);
            removed.push(info.session_id);
        }
    }
    removed
}

/// Ensure the runtime dir exists (idempotent) with private (0700) perms.
pub fn ensure_runtime_dir() -> std::io::Result<()> {
    let dir = identity::runtime_dir();
    std::fs::create_dir_all(&dir)?;
    identity::set_private_dir(&dir)?;
    Ok(())
}
