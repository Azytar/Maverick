//! `Signal::install` reports the dispositions it could not install.
//!
//! This lives in its own test binary for a reason that is easy to get wrong:
//! `install` always sets `SIGCHLD` to `SA_NOCLDWAIT`, which is a *process-wide*
//! disposition that makes the kernel reap children automatically. A test that
//! calls it therefore changes the reaping behaviour of every other test in the
//! same binary — including `a_zombie_is_the_same_process_but_not_a_running_one`,
//! which needs a child to stay unreaped. That is not a hypothetical: it is what
//! happened when these tests lived beside it, and it is the same class of
//! process-global state that the reply fixtures were split out for.

use maverick_sys::Signal;

/// The disposition mask the kernel currently has, read back from `/proc` so the
/// assertions are the kernel's view rather than the library's own return value.
fn caught_mask() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("status");
    for line in status.lines() {
        if let Some(hex) = line.strip_prefix("SigCgt:") {
            return u64::from_str_radix(hex.trim(), 16).expect("hex mask");
        }
    }
    panic!("no SigCgt in /proc/self/status");
}

fn the_real_chain() -> Signal {
    Signal::new()
        .ignore(libc::SIGPIPE)
        .on_sigterm(libc::SIGTERM)
        .on_sigterm(libc::SIGINT)
        .on_sigcont(libc::SIGCONT)
}

#[test]
fn a_good_install_reports_nothing_and_really_installs() {
    let failed = the_real_chain().install();
    assert!(
        failed.is_empty(),
        "a good install must report nothing, got {failed:?}"
    );
    let mask = caught_mask();
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGCONT] {
        assert!(
            mask & (1u64 << (sig - 1)) != 0,
            "signal {sig} should be caught; SigCgt={mask:#x}"
        );
    }
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
