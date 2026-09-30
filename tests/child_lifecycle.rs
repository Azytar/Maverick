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
//!
//! `no_wait_in_wm.rs` covers the other half of the rule — that no code linked
//! into `maverick` calls a wait at all.
//!
//! The order matters: every assertion here runs in a process that has already
//! installed the production dispositions, because that is the only state in
//! which the contract is meaningful.

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
