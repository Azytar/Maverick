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
/// Each entry is enriched: if the ficha's `display`/`tty_nr` are empty we fall
/// back to reading `/proc/<pid>/environ` and `/proc/<pid>/stat`. `alive` is set
/// by pinging the socket; sockets that reject connections are flagged stale
/// (the WM died without cleanup) and their ficha is left for the caller.
pub fn list_instances() -> Vec<InstanceInfo> {
    let dir = identity::runtime_dir();
    let mut out = Vec::new();

    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return out,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let name = match path.file_stem().and_then(|s| s.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let mut info = match identity::read_meta(&name) {
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

        // Probe liveness via the socket.
        info.alive = control::ping(&name).is_ok();

        out.push(info);
    }

    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Find one instance by exact name.
pub fn find_by_name(name: &str) -> Option<InstanceInfo> {
    list_instances().into_iter().find(|i| i.name == name)
}

/// Find instances whose display matches (e.g. ":1").
pub fn find_by_display(display: &str) -> Vec<InstanceInfo> {
    list_instances()
        .into_iter()
        .filter(|i| i.display == display)
        .collect()
}

/// Ask a single instance (by name) to quit via its control socket.
/// Returns the server reply or an error if it can't be reached.
pub fn quit_by_name(name: &str) -> std::io::Result<String> {
    let reply = control::quit(name)?;
    // Socket answered the quit; clean up the ficha too.
    identity::cleanup_meta(name);
    Ok(reply)
}

/// Quit every discovered instance that is still alive.
/// Returns a summary of (name, result).
pub fn quit_all() -> Vec<(String, std::io::Result<String>)> {
    let live: Vec<String> = list_instances()
        .into_iter()
        .filter(|i| i.alive)
        .map(|i| i.name)
        .collect();
    live.into_iter()
        .map(|name| {
            let r = quit_by_name(&name);
            (name, r)
        })
        .collect()
}

/// Remove stale fichas whose socket no longer answers. Returns the names removed.
pub fn prune_stale() -> Vec<String> {
    let mut removed = Vec::new();
    for info in list_instances() {
        if !info.alive {
            identity::cleanup_meta(&info.name);
            removed.push(info.name);
        }
    }
    removed
}

/// Ensure the runtime dir exists (idempotent).
pub fn ensure_runtime_dir() -> std::io::Result<()> {
    fs::create_dir_all(identity::runtime_dir())
}
