//! Structural guard for the one rule of Maverick's process model that the
//! compiler cannot express: **no code linked into the `maverick` binary may
//! block on a child's exit status.**
//!
//! `maverick-sys` installs `SA_NOCLDWAIT` on `SIGCHLD` at startup, before the
//! first child can exist, so the kernel discards every child's exit status. A
//! `wait` in that process therefore cannot obtain a status — and, measured on
//! Linux, it does not fail fast either: it blocks for the child's entire
//! remaining life and *then* returns `ECHILD`. The damage is the block, on the
//! window manager's own event-loop thread, for a duration the child chooses.
//!
//! `tests/child_lifecycle.rs` proves the *consequence* of the rule at runtime,
//! by observing that a child of this process cannot be waited for at all. This
//! test proves the *absence of the cause* at build time, so the regression is
//! caught by `cargo test` in a fraction of a second rather than only when some
//! child happens to hang.
//!
//! The rule spans a boundary that is now a crate boundary. `maverick-sys` is
//! linked into both binaries, but the waiting code lives in `maverickctl`,
//! which is never linked into the `maverick` window manager and legitimately
//! waits (`session exec --wait` is specified to block and report the status,
//! and `maverickctl` never installs `SA_NOCLDWAIT`). The walked directories
//! below therefore need no exemptions: `ALLOWED` is empty on purpose, and an
//! entry may only ever be added with the reason waiting there is sound. Any
//! crate gaining a `wait` in a path that reaches the WM is a violation, and a
//! crate that reaches the WM without depending on `maverick-sys` cannot be
//! protected by any convention in `maverick-sys` at all. That is the reason
//! this test is a source scan rather than an architectural note.

use std::fs;
use std::path::{Path, PathBuf};

/// Calls that block on, or poll for, a child's exit status, paired with the
/// human-readable name reported in a failure. Matched as call-shaped substrings
/// so that a variable receiver still matches, e.g. `convert.output()`.
const FORBIDDEN: &[(&str, &str)] = &[
    (".wait()", "Child::wait"),
    (".wait_with_output()", "Child::wait_with_output"),
    (".try_wait()", "Child::try_wait"),
    (".output()", "Command::output"),
    (".status()", "Command::status"),
    ("libc::wait(", "libc::wait"),
    ("libc::waitpid", "libc::waitpid"),
    ("libc::waitid", "libc::waitid"),
    ("Child::status", "Child::status"),
];

/// Source directories of every crate linked into the `maverick` binary, listed
/// as **directory paths** rather than crate names.
///
/// This is not a stylistic choice. A first draft of this test iterated crate
/// names and joined `"src"` onto each, so for the root binary crate it built
/// `maverick/src` — a path that does not exist. The walk found nothing, found
/// nothing to complain about, and reported success on a tree that violated the
/// rule in `src/main.rs`, silently skipping the window manager's own 42 source
/// files. Enumerating real paths makes that class of mistake a non-zero
/// assertion instead of a silent pass.
/// Enumerating real paths makes that class of mistake a non-zero
/// assertion instead of a silent pass — which is how this list earned its
/// current contents. It used to name `maverick-gl/src` and `maverick-vk/src`,
/// neither of which is linked into the WM, while omitting `maverick-toml`, which
/// is. The walk found nothing to complain about and reported success over a
/// short list.
///
/// This is the dependency set of the `maverick` binary. Update it whenever a
/// dependency is added or removed: the missing-directory assertion below is
/// what makes that failure loud rather than silent.
const WM_SOURCE_DIRS: &[&str] = &[
    "src",
    "maverick-core/src",
    "maverick-sys/src",
    "maverick-toml/src",
    "maverick-x11/src",
];

/// Paths permitted to wait, each with the reason it is sound.
///
/// Deliberately empty: every directory the rule walks is linked into the
/// window manager, and none of that code may wait. The legitimately waiting
/// code lives in `maverickctl`, which is never linked into `maverick` and
/// therefore never walked.
const ALLOWED: &[(&str, &str)] = &[];

/// Strip `#[cfg(test)]` regions and line comments, returning the remaining
/// lines with their original 1-based numbers.
///
/// Both exclusions are load-bearing, and getting either wrong is silent:
///
/// * `#[cfg(test)]` regions. An in-file `#[cfg(test)] mod tests { .. }` is
///   allowed to wait — a test may legitimately assert that waiting *fails*.
///   A naive terminator that looks for `{` on the `}` line never matches
///   `mod tests { ... }`, so the region never closes and every line below the
///   first `#[cfg(test)]` in the file is excused. In `session/proc.rs` that is
///   most of the file, which made an early draft of this test pass on a
///   violation placed below it.
/// * Line comments. A doc comment describing `SA_NOCLDWAIT` necessarily
///   contains the word `wait`; scanning it would make this test fail on its
///   own documentation and train reviewers to ignore it.
fn scrubbed_source(src: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut in_test_cfg = false;
    // Brace depth *before* the line that closed the test module, so that a
    // `}` on its own line ends the region instead of counting as one level in.
    let mut depth: i32 = 0;

    for (idx, raw) in src.lines().enumerate() {
        let line = raw.trim_start();

        if !in_test_cfg && (line.starts_with("#[cfg(test)]") || line.starts_with("#[cfg(all(test"))
        {
            in_test_cfg = true;
            // The attribute line is not part of the module body, so the depth
            // starts at zero and only lines *after* it can close the region.
            depth = 0;
        } else if in_test_cfg {
            let opens = raw.matches('{').count() as i32;
            let closes = raw.matches('}').count() as i32;
            if (opens > 0 || closes > 0) && depth + opens - closes <= 0 {
                in_test_cfg = false;
                depth = 0;
                continue;
            }
            depth += opens - closes;
            continue;
        }

        let code = match line.find("//") {
            Some(at) => &raw[..at],
            None => raw,
        };
        out.push((idx + 1, code.to_string()));
    }
    out
}

fn collect_rs_files(dir: &Path, acc: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, acc);
        } else if path.extension().is_some_and(|e| e == "rs") {
            acc.push(path);
        }
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn is_allowed(rel: &str) -> bool {
    ALLOWED.iter().any(|(prefix, _)| rel.starts_with(prefix))
}

#[test]
fn no_code_linked_into_the_window_manager_waits_on_a_child() {
    let root = repo_root();
    let mut violations: Vec<String> = Vec::new();
    let mut scanned_total = 0usize;

    for dir in WM_SOURCE_DIRS {
        let abs = root.join(dir);
        assert!(
            abs.is_dir(),
            "source directory {dir} does not exist under {}; this test's \
             coverage of it has silently become zero",
            root.display()
        );

        let mut files = Vec::new();
        collect_rs_files(&abs, &mut files);
        assert!(
            !files.is_empty(),
            "source directory {dir} contains no .rs files; the scan is vacuous"
        );
        assert!(
            files.iter().all(|f| f.starts_with(&abs)),
            "the walk for {dir} escaped its own directory into {}",
            files
                .iter()
                .find(|f| !f.starts_with(&abs))
                .map(|f| f.display().to_string())
                .unwrap_or_default()
        );

        for file in &files {
            let rel = file
                .strip_prefix(&root)
                .unwrap_or(file)
                .to_string_lossy()
                .replace('\\', "/");
            if is_allowed(&rel) {
                continue;
            }
            let Ok(src) = fs::read_to_string(file) else {
                continue;
            };
            scanned_total += 1;
            for (lineno, line) in scrubbed_source(&src) {
                for (needle, label) in FORBIDDEN {
                    if line.contains(needle) {
                        violations.push(format!(
                            "{rel}:{lineno}: {label} — no code linked into the \
                             `maverick` binary may block on a child, because \
                             SA_NOCLDWAIT discards the status and a wait only \
                             stalls the event loop for the child's lifetime"
                        ));
                    }
                }
            }
        }
    }

    assert!(
        !violations.is_empty() || scanned_total > 0,
        "the scan examined no files at all; a passing result here would mean \
         nothing, not that the rule holds"
    );
    assert!(
        violations.is_empty(),
        "the window-manager process must not wait on children:\n{}",
        violations.join("\n")
    );
}

/// The detector itself has to be capable of failing, or the test above is a
/// tautology. This re-runs the same needles against the one file that is
/// allowed to wait, and requires the scan to find it. If a
/// future edit to `FORBIDDEN`, `scrubbed_source`, or the walk quietly stops
/// matching, this fails while the rule test still passes.
#[test]
fn the_wait_detector_can_still_see_a_real_violation() {
    let root = repo_root();
    let control = root.join("maverickctl/src/ctl/session.rs");
    assert!(
        control.is_file(),
        "the allow-listed control file moved; re-derive the ALLOWED entry and \
         this self-check together"
    );

    let src = fs::read_to_string(&control).expect("read control file");
    let hits: Vec<&str> = FORBIDDEN
        .iter()
        .filter(|(needle, _)| src.lines().any(|l| l.contains(needle)))
        .map(|(_, label)| *label)
        .collect();

    assert!(
        !hits.is_empty(),
        "no forbidden call found in {}, but maverickctl is documented as \
         legitimately waiting. Either the file changed or the detector is \
         blind; both mean this test can no longer detect a violation.",
        control.display()
    );
}
