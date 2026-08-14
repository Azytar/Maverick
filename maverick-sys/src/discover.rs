// maverick-sys/src/discover.rs
// Discovery + remote control for Maverick instances.
//
// Scans the per-user runtime dir for `*.json` fichas, enriches them with live
// /proc data (DISPLAY, tty_nr), and offers operations to quit one or all
// instances by name. This is what lets a tool tell three Mavericks on three
// different TTYs/DISPLAYs apart and target the right one.

use std::fs;

use crate::control;
use crate::identity::{self, InstanceInfo};

/// List every Maverick instance with a ficha on disk.
///
/// Each session lives in its own subdirectory of `runtime_dir()` named after
/// its `session_id`; the ficha is `<sid>/<sid>.json`. Each entry is enriched:
/// if the ficha's `display`/`tty_nr` are empty we fall back to reading
/// `/proc/<pid>/environ` and `/proc/<pid>/stat`. `alive` requires both that the
/// socket answers a ping *and* that the recorded pid is still the same process
/// (its `/proc/<pid>/stat` start time matches ours), so a crashed instance
/// whose PID was later recycled is not mistaken for live.
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
        if !path.is_dir() {
            continue;
        }
        let sid = match path.file_name().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let mut info = match identity::read_meta(&sid) {
            Some(i) => i,
            None => continue,
        };

        // Enrich with live /proc data so we can distinguish TTYs/DISPLAYs.
        if info.display.is_empty() {
            info.display = identity::read_proc_environ_display(info.pid);
        }
        if info.tty_nr == 0 {
            info.tty_nr = identity::read_proc_tty(info.pid);
        }
        if info.exe.is_empty() {
            info.exe = identity::read_proc_exe(info.pid);
        }

        // Robust liveness via socket + PID/start_time match.
        info.alive = is_instance_alive(&info);

        out.push(info);
    }

    out.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    out
}

/// True if the instance's socket answers a ping *and* the recorded pid is still
/// the same process (its start time matches). Guards against PID reuse after a
/// crash leaving a stale ficha (the socket's own stale socket was already
/// unlinked by a TOCTOU-safe check at spawn, but a SIGKILL'd instance may still
/// have a dead socket lying around that rejects connections).
fn is_instance_alive(info: &InstanceInfo) -> bool {
    // 1. The socket must answer a ping.
    if control::ping(&info.session_id).is_err() {
        return false;
    }
    // 2. The pid must still exist and be the *same* process we recorded.
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

/// Find instances whose display matches (e.g. ":1").
pub fn find_by_display(display: &str) -> Vec<InstanceInfo> {
    list_instances()
        .into_iter()
        .filter(|i| i.display == display)
        .collect()
}

/// Ask a single instance (by session id) to quit via its control socket.
/// Returns the server reply or an error if it can't be reached.
pub fn quit_by_name(sid: &str) -> std::io::Result<String> {
    let reply = control::quit(sid)?;
    // Socket answered the quit; clean up the ficha too.
    identity::cleanup_meta(sid);
    Ok(reply)
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
