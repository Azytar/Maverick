//! The window manager's child-reaping contract, pinned from the outside.
//!
//! This lives in its own test binary for the same reason
//! `maverick-sys/tests/signal_install.rs` does: [`maverick_sys::Signal::install`]
//! changes `SIGCHLD` to `SA_NOCLDWAIT`, which is a *process-wide* disposition.
//! Installing it here would silently change the reaping behaviour of every
//! other test in the same binary, so it must never be merged into one.
//!
//! What is pinned, in the order the window manager does it:
//!
//! 1. `Signal::install` really does set `SA_NOCLDWAIT`, and the consequence is
//!    that **no** child of this process can be waited for — `waitpid` reports
//!    `ECHILD` even for a child that is still running.
//! 2. That is exactly right for the window manager's own children (autostart
//!    clients and keybind-spawned programs), which are never waited on and are
//!    therefore better auto-reaped than left as zombies.
//! 3. It is *not* right for anything in the same process that needs a child's
//!    exit status. `maverick_img` is that thing: its external fallback for
//!    formats with no native decoder runs a converter. This test proves that
//!    path keeps working under `SA_NOCLDWAIT`.
//!
//! The order matters: every assertion here runs in a process that has already
//! installed the production dispositions, because that is the only state in
//! which the contract is meaningful.

use std::io::Read;

/// Install exactly the dispositions the window manager installs at startup.
///
/// Written out rather than delegated so this binary asserts the *contract*, not
/// that `Signal` happens to produce the same list twice; the entry that matters
/// is `SIGCHLD`, and only `Signal::install` may set that one.
fn install_window_manager_signals() {
    let uninstalled = maverick_sys::Signal::new()
        .ignore(libc::SIGPIPE)
        .on_sigterm(libc::SIGTERM)
        .on_sigterm(libc::SIGINT)
        .on_sigterm(libc::SIGQUIT)
        .on_sigcont(libc::SIGCONT)
        .install();
    assert!(
        uninstalled.is_empty(),
        "the production dispositions must install in an unconfined test process: {uninstalled:?}"
    );
}

/// `std::process::Child::wait`, `Child::try_wait` and
/// `std::process::Command::output` all go through `waitpid(2)`, which
/// `SA_NOCLDWAIT` makes pointless for any child of this process: the status is
/// discarded by the kernel and the wait reports `ECHILD` instead.
#[test]
fn a_child_of_this_process_cannot_be_waited_for() {
    install_window_manager_signals();

    // A child that exits on its own. `wait` may block until the child is gone
    // before reporting the error, so pinning the child to a fixed lifetime
    // would make this test's runtime depend on it; the contract under test is
    // the error, not the timing.
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn");
    let waited = child.wait();
    let _ = child.kill();

    let err = waited.expect_err("SA_NOCLDWAIT must make waiting impossible");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ECHILD),
        "waiting under SA_NOCLDWAIT must fail with ECHILD, got {err}"
    );
}

/// The autostart and keybind-spawn paths never wait, so `SA_NOCLDWAIT` is
/// their whole reaping strategy: a dropped `Child` leaves no `/proc` entry.
#[test]
fn a_spawned_child_leaves_no_zombie_without_an_explicit_reap() {
    install_window_manager_signals();

    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn");
    let pid = child.id();
    // `Child` is dropped here, exactly as `src/main.rs` and
    // `src/backend/x11/actions.rs` drop theirs.
    drop(child);

    // Polled to a deadline rather than slept: the assertion is "it goes away",
    // and how long the kernel takes is the kernel's business, not the test's.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        let state = stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next());
        assert_ne!(
            state,
            Some("Z"),
            "a dropped Child must not become a zombie under SA_NOCLDWAIT"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "the auto-reaped child was still present after 5s"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// `maverick_img::decode` delegates every format without a native decoder —
/// JPEG, WebP, AVIF — plus any native decode failure, to an external
/// converter. That fallback used `Command::output()`, i.e. `waitpid`, which
/// `SA_NOCLDWAIT` makes impossible; inside the running window manager the
/// fallback therefore failed with `ECHILD` for every such file.
#[test]
fn the_external_image_fallback_does_not_depend_on_a_childs_exit_status() {
    install_window_manager_signals();

    // ImageMagick probes by content, not by extension, so a PNG wearing a
    // `.jpg` name exercises the delegated path end to end without adding a
    // fixture the repository does not already have. ffmpeg keys off the name
    // and rejects it, which is fine: the fallback tries each converter in turn.
    if resolve_converter("convert").is_none() && resolve_converter("magick").is_none() {
        let err = maverick_img::decode(std::path::Path::new("/nonexistent.jpg"))
            .expect_err("a missing file must not decode");
        assert_no_wait_failure(&err);
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("maverick-img/tests/fixtures/rgb3x1.png");
    let disguised = dir.path().join("delegated.jpg");
    std::fs::copy(&source, &disguised).expect("copy fixture");

    let decoded = maverick_img::decode(&disguised).unwrap_or_else(|e| {
        panic!("the external converter path must work under SA_NOCLDWAIT: {e}")
    });

    assert_eq!((decoded.w, decoded.h), (3, 1), "dimensions must round-trip");
    assert_eq!(decoded.data.len(), 3 * 4, "one RGBA pixel per source pixel");
    // Alpha exists only if the converter's PPM was re-widened to RGBA here,
    // and the three pixels differ only if the converter's real output came
    // through rather than a constant fill. The channel *values* are
    // deliberately not asserted. The fixture is a PNG carrying no gAMA, cHRM,
    // iCCP or sRGB chunk, so it declares no colour space and both decoders
    // return its stored samples verbatim — there is no colour management for a
    // converter to apply, and on the reference toolchain the two paths agree
    // byte for byte. Pinning the values would still be wrong, because it would
    // make this test assert against whichever converter the host happens to
    // have installed, when what it exists to prove is that the delegated path
    // completes under `SA_NOCLDWAIT` without a waitable child. Pixel equality
    // across the two decoders is covered in `maverick-img` against an
    // independent model of the PNG spec, not here.
    for px in decoded.data.chunks_exact(4) {
        assert_eq!(px[3], 0xff, "every pixel must be opaque");
    }
    assert_ne!(
        decoded.data[0..3],
        decoded.data[4..7],
        "the fixture's pixels differ, so a constant fill means the converter's \
         output was not what got parsed"
    );
}

/// A missing file takes the same code path but ends in the error arm, so it is
/// the cheapest proof that the error text reports a converter problem rather
/// than a wait failure.
#[test]
fn a_failed_external_decode_does_not_report_a_wait_failure() {
    install_window_manager_signals();
    if !["ffmpeg", "convert", "magick"]
        .iter()
        .any(|name| resolve_converter(name).is_some())
    {
        return;
    }
    let err = maverick_img::decode(std::path::Path::new("/nonexistent.jpg"))
        .expect_err("a missing file must not decode");
    assert_no_wait_failure(&err);
    assert!(
        err.starts_with("maverick-img:"),
        "unexpected error shape: {err}"
    );
}

fn assert_no_wait_failure(err: &str) {
    let lower = err.to_ascii_lowercase();
    assert!(
        !(lower.contains("no child") || lower.contains("echild") || lower.contains("os error 10")),
        "a converter failure must be reported as a converter failure, not as \
         ECHILD from a wait this process is not allowed to make: {err}"
    );
}

/// The same `PATH` scan `maverick_img` uses, so the test's idea of "is a
/// converter installed" cannot drift from the crate's.
fn resolve_converter(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(name);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(&candidate)
                .ok()
                .filter(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                .map(|_| candidate)
        }
        #[cfg(not(unix))]
        {
            std::fs::metadata(&candidate).ok().map(|_| candidate)
        }
    })
}

/// The shape `decode_external` uses to collect a converter's output: read the
/// pipe to EOF, drop the `Child`, never wait. Reading to EOF is what
/// synchronises the child, so it has to work with no waitable child at all.
#[test]
fn a_bounded_pipe_read_does_not_depend_on_the_childs_status() {
    install_window_manager_signals();
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf 'P6\\n1 1\\n255\\n\\000\\000\\000'"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn");
    let mut buf = Vec::new();
    child
        .stdout
        .take()
        .expect("piped stdout")
        .read_to_end(&mut buf)
        .expect("read to EOF");
    drop(child);
    assert_eq!(buf, b"P6\n1 1\n255\n\0\0\0");
}
