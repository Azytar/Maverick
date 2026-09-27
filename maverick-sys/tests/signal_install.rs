//! `Signal::install` reports the dispositions it could not install, and the
//! dispositions it does install behave as the window manager depends on.
//!
//! This lives in its own test binary for a reason that is easy to get wrong:
//! `install` always sets `SIGCHLD` to `SA_NOCLDWAIT`, which is a *process-wide*
//! disposition that makes the kernel reap children automatically. A test that
//! calls it therefore changes the reaping behaviour of every other test in the
//! same binary — including `a_zombie_is_the_same_process_but_not_a_running_one`,
//! which needs a child to stay unreaped. That is not a hypothetical: it is what
//! happened when these tests lived beside it, and it is the same class of
//! process-global state that the reply fixtures were split out for.
//!
//! Everything here runs in one process, so the assertions that need a signal
//! delivered *to this process* — the quit and regrab flags — are written to be
//! order-independent: each clears the flag it is about to test, then waits for
//! it to be observed, rather than assuming it started false.

use maverick_sys::Signal;

/// The disposition mask the kernel currently has, read back from `/proc` so the
/// assertions are the kernel's view rather than the library's own return value.
fn caught_mask() -> u64 {
    proc_signal("SigCgt:")
}

/// The mask of signals this process is *ignoring* rather than handling.
fn ignored_mask() -> u64 {
    proc_signal("SigIgn:")
}

fn proc_signal(field: &str) -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("status");
    for line in status.lines() {
        if let Some(hex) = line.strip_prefix(field) {
            return u64::from_str_radix(hex.trim(), 16).expect("hex mask");
        }
    }
    panic!("no {field} in /proc/self/status");
}

fn the_real_chain() -> Signal {
    Signal::new()
        .ignore(libc::SIGPIPE)
        .on_sigterm(libc::SIGTERM)
        .on_sigterm(libc::SIGINT)
        .on_sigterm(libc::SIGQUIT)
        .on_sigcont(libc::SIGCONT)
}

/// Poll `flag` to a deadline rather than sleeping: the property is "the handler
/// ran", and how quickly the kernel gets round to it is not the test's business.
fn wait_for(mut flag: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if flag() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// Deliver a signal to this process. Process-directed on purpose: a window
/// manager must work whichever thread the kernel picks, so the test must too.
fn raise(sig: libc::c_int) {
    // SAFETY: `raise` takes a signal number and no pointers. `libc::raise`
    // only ever fails for an invalid signal, and a panic would be the honest
    // outcome for a constant this crate supplies.
    assert_eq!(unsafe { libc::raise(sig) }, 0, "raise {sig}");
}

#[test]
fn a_good_install_reports_nothing_and_really_installs() {
    let failed = the_real_chain().install();
    assert!(
        failed.is_empty(),
        "a good install must report nothing, got {failed:?}"
    );
    let mask = caught_mask();
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGQUIT, libc::SIGCONT] {
        assert!(
            mask & (1u64 << (sig - 1)) != 0,
            "signal {sig} should be caught; SigCgt={mask:#x}"
        );
    }
}

/// An ignored signal is not a handled one, and the difference decides whether a
/// client hanging up mid-write can take the window manager with it. Reading it
/// off `SigIgn` rather than off the builder means the assertion cannot be
/// satisfied by the library agreeing with itself.
///
/// `SIGPIPE` is reset to `SIG_DFL` first, because the Rust runtime already
/// ignores it before `main` runs: without that, the assertion would pass whether
/// or not `install` did anything, which is precisely the mutation worth
/// catching here.
#[test]
fn an_ignored_signal_is_really_ignored() {
    reset(libc::SIGPIPE, libc::SIG_DFL);
    assert_eq!(
        ignored_mask() & (1u64 << (libc::SIGPIPE - 1)),
        0,
        "the test needs SIGPIPE to start at its default disposition"
    );

    the_real_chain().install();

    let mask = ignored_mask();
    assert!(
        mask & (1u64 << (libc::SIGPIPE - 1)) != 0,
        "install must ignore SIGPIPE itself; SigIgn={mask:#x}"
    );
    assert_eq!(
        mask & (1u64 << (libc::SIGTERM - 1)),
        0,
        "a signal with a handler must not also be ignored"
    );
}

/// Put a signal back to a bare disposition, so an assertion about what `install`
/// does is not answered by whatever the runtime happened to leave behind.
fn reset(sig: libc::c_int, action: usize) {
    // SAFETY: the struct is zero-initialised, the action written to the handler
    // member and the mask emptied before the call, and a null `oldact` is
    // always valid. This is a test-only reset of a signal this binary owns.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = action;
        libc::sigemptyset(&mut sa.sa_mask);
        assert_eq!(libc::sigaction(sig, &sa, std::ptr::null_mut()), 0);
    }
}

/// The whole reason `SA_NOCLDWAIT` is installed, and the reason a failure to
/// install it is reported: without it every autostarted client the window
/// manager starts stays a zombie for the life of the process. Dropping the flag
/// is a one-character change and nothing else in the crate would notice, so it
/// is asserted here against the kernel's own view.
#[test]
fn children_are_auto_reaped_rather_than_left_as_zombies() {
    the_real_chain().install();

    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn");
    let pid = child.id();
    drop(child);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        let state = stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next());
        assert_ne!(
            state,
            Some("Z"),
            "an unreaped child became a zombie: SA_NOCLDWAIT is not in effect"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "the child was never reaped"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// A handler that stores nothing is a handler that is not installed, however
/// convincingly `SigCgt` says otherwise — the kernel only records that *a*
/// handler exists. This is the one place the flag's round trip is checked.
#[test]
fn a_stop_signal_reaches_the_quit_flag_and_clearing_it_is_observed() {
    the_real_chain().install();
    maverick_sys::clear_quit();
    assert!(!maverick_sys::quit_requested(), "the flag must start clear");

    raise(libc::SIGTERM);
    assert!(
        wait_for(maverick_sys::quit_requested),
        "SIGTERM must set the quit flag"
    );

    maverick_sys::clear_quit();
    assert!(
        !maverick_sys::quit_requested(),
        "clearing the flag must be observable, or a second signal is indistinguishable"
    );
}

/// SIGINT and SIGQUIT reach the same flag, because the window manager treats them
/// as the same request. They are also the two dispositions a backgrounded job
/// inherits as `SIG_IGN`, so they are the ones whose handlers must exist.
#[test]
fn sigint_and_sigquit_reach_the_same_quit_flag() {
    the_real_chain().install();
    for sig in [libc::SIGINT, libc::SIGQUIT] {
        maverick_sys::clear_quit();
        raise(sig);
        assert!(
            wait_for(maverick_sys::quit_requested),
            "signal {sig} must set the quit flag"
        );
    }
}

#[test]
fn sigcont_reaches_the_regrab_flag() {
    the_real_chain().install();
    maverick_sys::clear_regrab();
    assert!(!maverick_sys::need_regrab(), "the flag must start clear");

    raise(libc::SIGCONT);
    assert!(
        wait_for(maverick_sys::need_regrab),
        "SIGCONT must set the regrab flag"
    );

    maverick_sys::clear_regrab();
    assert!(!maverick_sys::need_regrab(), "clearing must be observable");
}

#[test]
fn a_repeat_install_is_not_mistaken_for_a_failure() {
    // The result reports refusals, not redundancy: installing twice is legal
    // and must not look like something went wrong.
    assert!(the_real_chain().install().is_empty());
    assert!(the_real_chain().install().is_empty());
}

#[test]
fn an_invalid_signal_is_reported_rather_than_swallowed() {
    // The case the discarded result used to hide: the kernel refuses, and the
    // caller used to believe it had a handler for a signal that does not exist.
    let failed = Signal::new().on_sigterm(-1).install();
    assert_eq!(failed, vec![-1], "a refused signal must be reported");
}
