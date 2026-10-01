//! The private runtime directory one `maverickctl` test binary runs against.
//!
//! `XDG_RUNTIME_DIR` is a process-global the CLI reads to discover instances, so
//! every test binary has to point it at a directory of its own. Without that,
//! the properties here would depend on whether the machine running them happens
//! to have a live window manager — and `list` and `prune` would read and delete
//! the developer's real sessions.
//!
//! Each binary passes its own `prefix` so a stray directory in `$TMPDIR` is
//! attributable to the binary that left it, and the process id is part of the
//! name so two binaries running at once cannot land on the same path. Both are
//! read back only through the environment, never by name, so the namespace is
//! documentation and the process id is what actually keeps runs apart.
//!
//! A subdirectory rather than a `runtime_dir.rs` beside the binaries: cargo
//! compiles every `tests/*.rs` as its own test target, so a flat file here would
//! add an empty binary. `tests/common/` is the same arrangement.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// The path this binary's runtime directory takes, for `prefix`.
///
/// Pure: it computes a name and touches nothing, so a test can compare what two
/// prefixes or two processes would resolve to without creating either. The
/// process id is what makes it distinct per binary; the prefix makes it legible.
pub fn dir_for(prefix: &str) -> PathBuf {
    std::env::temp_dir().join(format!("{prefix}-{}", std::process::id()))
}

/// Point `XDG_RUNTIME_DIR` at this binary's private directory, creating it, and
/// return the path.
///
/// Set **once per prefix per process**: the tests in a binary run in parallel and
/// must agree on one directory, because the fixture socket and the record
/// written for it have to land in the same place. A test that needs a *different*
/// directory mid-binary — one that has to observe a command resolving nothing,
/// say — moves the variable itself, and must not come through here, or the next
/// test to ask for the binary's directory would move the target back out from
/// under it.
///
/// Keyed by prefix rather than initialised once, because a single cell would
/// hand every later caller the first caller's directory: a second namespace
/// would silently get the first one's fixtures, which is the one thing a
/// per-binary directory exists to prevent.
///
/// An existing directory is reused rather than emptied. A recycled process id
/// can therefore land on a directory a previous run left, and this does not
/// defend against that; a binary that needs a guaranteed-empty directory says so
/// and creates one itself.
pub fn isolate(prefix: &str) -> PathBuf {
    static DIRS: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
    let mut dirs = DIRS
        .get_or_init(Mutex::default)
        .lock()
        .expect("runtime directory map is never held across a panic");
    dirs.entry(prefix.to_string())
        .or_insert_with(|| {
            let dir = dir_for(prefix);
            let _ = std::fs::create_dir_all(&dir);
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
            dir.clone()
        })
        .clone()
}
