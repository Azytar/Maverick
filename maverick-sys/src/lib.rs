//! System boundary — process, signal and session FFI in one place.
//!
//! Centralizes this crate's `libc` FFI: POSIX signal handlers (`sigaction`),
//! `poll(2)` for the event-loop socket, `getuid`/`getgid` identity reads, and
//! the process-tree signalling (`kill`/`killpg`) behind [`session`]. The event
//! loop polls the [`AtomicBool`] flags exported here, and no `static mut` is
//! used anywhere in the workspace.
//!
//! `unsafe` is **not** confined to this crate. `maverick-gl`, `maverick-vk` and
//! `maverick-x11` each contain their own FFI `unsafe`, as does the X11 backend
//! in the root package; within those crates it stays confined to the FFI
//! boundary and the public API is safe. `maverick-core` is `unsafe`-free: it
//! speaks `WindowId(u32)` and `Rect` and holds no FFI.
//!
//! What is not owned: the X11 connection fd passed to [`wait_readable_fds`], the
//! terminal fds touched by [`detach_from_terminal`], and the WM state itself
//! (the control channel only queues commands via [`ControlHub`]).
//!
//! Submodules:
//! - [`control`] — per-session `UnixListener` at [`identity::sock_path`],
//!   line-based protocol (`ping`/`identify`/`state`/`dispatch`/`quit`/
//!   `restart`/`reload`/`subscribe`/`query`), background accept thread, talks
//!   to the WM only through [`hub::ControlHub`].
//! - [`hub`] — `Arc`/`Mutex`/`mpsc` bridge between the server thread and the
//!   single WM thread: command queue, cached state snapshot, and `subscribe`
//!   event sinks. Cloning is cheap and shares the same queues.
//! - [`discover`] — scans [`identity::runtime_dir`] fichas, enriches with live
//!   `/proc` data (`DISPLAY`/`tty_nr`/`exe`), checks liveness via
//!   `ping` + `start_time` against PID reuse, and offers `quit`/`prune`.
//! - [`identity`] — [`InstanceInfo`], `session_id` generation, `runtime_dir`/
//!   `session_dir`/`sock_path`/`meta_path` (0700, fixed `control.sock` under
//!   `SUN_LEN`), `/proc/<pid>/stat`/`environ`/`exe` readers, and minimal JSON
//!   ficha I/O without `serde`.
//! - [`json`] — canonical `json_escape`/`json_quote`/`json_unescape` plus the
//!   flat-object codec `scan_object` used by `identity` and `session`; single
//!   copy, no `serde`.
//! - [`session`] — the Maverick Session model: a named, reproducible graphical
//!   unit (X server + Maverick + applications) with its own runtime directory,
//!   cookie, logs and lifecycle. Owns display allocation, the nested X server
//!   backend, the process tree and the session record. Still no control-plane
//!   policy: that is `ctl`.
//! - [`ctl`] — the `maverickctl` engine: instance
//!   selection (`--session`/`--name`/`$MAVERICK_INSTANCE`/DISPLAY+TTY
//!   context/singleton), `list`/`state`/`query`/`msg`/`subscribe`/`quit`/
//!   `restart`/`reload`/`prune`, and confirmation via
//!   `zenity`/`kdialog`/TTY.
//!
//! # Ownership
//!
//! [`Signal`] owns the handler/ignore lists; [`Signal::install`] consumes it
//! and installs `SIGCHLD` (`SIG_DFL` with `SA_NOCLDWAIT|SA_RESTART`) plus the
//! configured handlers and ignores. Static flags (`QUIT_REQUESTED`,
//! `NEED_REGRAB`) are written by `extern "C"` trampolines and read by the WM
//! thread via [`quit_requested`]/[`need_regrab`]. [`ControlServer`] owns the
//! listener and a `stop` flag; [`ControlHub`] is shared via `Arc` between
//! server and WM threads. `detach_from_terminal` is called once at startup
//! before the X connection is opened; [`wait_readable_fds`] is called each
//! event-loop iteration.
//!
//! Two properties of that arrangement are worth stating once, because callers
//! depend on them and neither is obvious:
//!
//! - **A handler is a notification, not a control path.** A trampoline only
//!   stores a process-global `AtomicBool`; the event loop reads it at the top of
//!   a turn and turns it into an ordinary method call. Nothing in a handler
//!   allocates, locks, logs or touches X11, and the flags are process-global so
//!   it does not matter which thread the kernel picks to run the trampoline on.
//!   A process-directed signal is still *delivered* to an arbitrary thread, so
//!   the loop's wake-up depends on the signal reaching the thread blocked in
//!   `poll`; `poll` is never restarted by `SA_RESTART` on Linux, so the
//!   resulting `EINTR` is what wakes it.
//! - **A control-socket command is the other control path, and it is not this
//!   flag.** `quit` arrives as a [`ControlCommand`] on the [`ControlHub`], not
//!   by setting `QUIT_REQUESTED`; the two converge on the caller's shutdown
//!   routine instead. The socket path is the stronger of the two, because the
//!   producer writes the hub's self-pipe, so the wake is caused rather than
//!   hoped for. There is deliberately no public setter for the quit flag, to
//!   keep a future reader from wiring the two together and losing that
//!   guarantee.
//!
//! # Safety
//!
//! There is exactly one disposition primitive, [`install_raw`], so the
//! `sigaction` struct layout, the handler-union member and the `sigemptyset` call
//! are argued once. It zero-initialises the struct, writes either a
//! `SIG_DFL`/`SIG_IGN` constant or the address of a permanently-linked
//! `extern "C" fn(c_int)`, passes an explicitly emptied `sa_mask`, and passes a
//! null `oldact`. Only `AtomicBool::store` with `SeqCst` runs inside a handler,
//! and `SeqCst` is stronger than the flag-to-flag hand-off needs. `poll` wraps
//! valid `pollfd`s and treats `EINTR`/errors as wakeups. `detach_from_terminal`
//! is best-effort, never calls `setsid`, and only redirects stdin/stdout to
//! `/dev/null` when `isatty(STDIN)` is true. The remaining `unsafe` in this
//! crate is confined to `getuid`/`getgid` identity reads and to the [`session`]
//! process-tree signals, which gate every `kill`/`killpg` on a recorded process
//! start time so a recycled PID cannot be hit.

use std::sync::atomic::{AtomicBool, Ordering};

/// Ordering used for the flag hand-offs between signal handler and event loop.
/// SeqCst keeps it simple and correct; these are rare, low-contention writes.
const ORD: Ordering = Ordering::SeqCst;

static QUIT_REQUESTED: AtomicBool = AtomicBool::new(false);
static NEED_REGRAB: AtomicBool = AtomicBool::new(false);

/// The poll timeout must survive being expressed as a `timespec`.
///
/// `tv_nsec` is a remainder under one billion, not a total, so a duration is
/// split rather than truncated. A timeout of zero has to return promptly
/// instead of blocking, because the event loop uses it for "no work pending".
#[cfg(test)]
mod poll_timeout_tests {
    use super::*;

    fn wait_on_idle(fd: std::os::unix::io::RawFd, d: std::time::Duration) -> bool {
        wait_readable_fds(&[fd], Some(d))
    }

    #[test]
    fn a_zero_timeout_returns_without_blocking() {
        // A pipe with nothing written: readable never becomes true.
        let (r, w) = std::os::unix::net::UnixStream::pair().expect("pair");
        let start = std::time::Instant::now();
        assert!(!wait_on_idle(
            std::os::unix::io::AsRawFd::as_raw_fd(&r),
            std::time::Duration::ZERO
        ));
        assert!(
            start.elapsed() < std::time::Duration::from_millis(500),
            "a zero timeout must not block"
        );
        drop(w);
    }

    #[test]
    fn a_timeout_longer_than_a_second_is_not_truncated_to_nanoseconds() {
        // Two seconds expressed as a nanosecond total would be an invalid
        // timespec. A short timeout is what we can actually observe, so this
        // only asserts the call returns rather than the kernel rejecting it.
        let (r, _w) = std::os::unix::net::UnixStream::pair().expect("pair");
        let fd = std::os::unix::io::AsRawFd::as_raw_fd(&r);
        let start = std::time::Instant::now();
        assert!(!wait_on_idle(fd, std::time::Duration::from_secs(2)));
        assert!(
            start.elapsed() >= std::time::Duration::from_millis(500),
            "the wait should have honoured a sub-second remainder, not returned instantly"
        );
    }

    /// A signal arriving during the block must wake the loop, not stall it.
    ///
    /// This is the property that keeps the window manager responsive to a
    /// control-socket command while it is idle on the X connection: `poll` is
    /// interrupted, and treating that as "nothing to do" would leave the command
    /// unprocessed until some unrelated event happened to arrive.
    #[test]
    fn a_signal_interrupting_the_wait_wakes_the_caller() {
        let (r, _w) = std::os::unix::net::UnixStream::pair().expect("pair");
        let fd = std::os::unix::io::AsRawFd::as_raw_fd(&r);
        // A handler with no observable effect, so the delivery is only about
        // the wait returning.
        // SAFETY: a no-op handler installed for a signal this test raises at
        // itself.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = sigusr1_trampoline as *const () as usize;
            sa.sa_flags = 0;
            libc::sigemptyset(&mut sa.sa_mask);
            assert_eq!(libc::sigaction(libc::SIGUSR1, &sa, std::ptr::null_mut()), 0);
        }
        // `tgkill` rather than `kill`: a signal is delivered to an arbitrary
        // thread that does not block it, so raising one process-wide would
        // usually interrupt some other test's thread and leave this one
        // blocked. Targeting the calling thread makes the delivery
        // deterministic.
        let pid = std::process::id() as libc::pid_t;
        let tid = unsafe { libc::gettid() };
        std::thread::spawn(move || {
            // Give the poll a moment to actually block, then interrupt it.
            std::thread::sleep(std::time::Duration::from_millis(50));
            // SAFETY: `tgkill(tgid, tid, sig)` aimed at the thread that is
            // blocked in poll, whose handler is installed above. Both ids came
            // from the kernel for this process and this thread.
            unsafe {
                libc::tgkill(pid, tid, libc::SIGUSR1);
            }
        });
        let start = std::time::Instant::now();
        // A long timeout: if EINTR were treated as "nothing happened", this
        // would block for the full second.
        assert!(wait_on_idle(fd, std::time::Duration::from_secs(2)));
        assert!(
            start.elapsed() < std::time::Duration::from_millis(1500),
            "the wait should have returned when interrupted, not run to its timeout"
        );
    }

    extern "C" fn sigusr1_trampoline(_: libc::c_int) {}

    #[test]
    fn a_readable_descriptor_is_reported() {
        let (r, mut w) = std::os::unix::net::UnixStream::pair().expect("pair");
        use std::io::Write;
        w.write_all(b"x").expect("write");
        assert!(wait_on_idle(
            std::os::unix::io::AsRawFd::as_raw_fd(&r),
            std::time::Duration::from_millis(500)
        ));
    }
}

/// True if a SIGTERM arrived and the WM should exit.
#[inline]
pub fn quit_requested() -> bool {
    QUIT_REQUESTED.load(ORD)
}

/// Clear the quit flag (call after acting on it).
#[inline]
pub fn clear_quit() {
    QUIT_REQUESTED.store(false, ORD);
}

/// True if a SIGCONT arrived and keyboard/pointer grabs must be redone.
#[inline]
pub fn need_regrab() -> bool {
    NEED_REGRAB.load(ORD)
}

/// Clear the regrab flag (call after the regrab succeeds).
#[inline]
pub fn clear_regrab() {
    NEED_REGRAB.store(false, ORD);
}

/// Builder for installing POSIX signal handlers without writing `sigaction`
/// structs by hand. Each method is safe; the FFI only happens inside `install()`.
pub struct Signal {
    handlers: Vec<Handler>,
    ignored: Vec<libc::c_int>,
}

enum Handler {
    Term(libc::c_int),   // set QUIT_REQUESTED
    Regrab(libc::c_int), // set NEED_REGRAB
}

impl Signal {
    /// Start a new signal configuration.
    pub fn new() -> Self {
        Signal {
            handlers: Vec::new(),
            ignored: Vec::new(),
        }
    }

    /// Ignore a signal entirely (e.g. SIGPIPE so a broken pipe can't kill the WM).
    pub fn ignore(mut self, sig: libc::c_int) -> Self {
        self.ignored.push(sig);
        self
    }

    /// On this signal, set the quit flag.
    ///
    /// Any signal that should mean "shut down" belongs here, not just
    /// `SIGTERM`: `SIGINT` and `SIGQUIT` take the same route, because the
    /// point of a handler is to replace the disposition the window manager
    /// inherited. A backgrounded job started by a non-interactive shell
    /// inherits `SIG_IGN` for both of those, and an ignored disposition
    /// survives `exec`, so without a handler of their own a backgrounded
    /// window manager cannot be stopped by `Ctrl-C` or `Ctrl-\` and would
    /// pass that same dead disposition to every client it starts.
    pub fn on_sigterm(mut self, sig: libc::c_int) -> Self {
        self.handlers.push(Handler::Term(sig));
        self
    }

    /// On this signal, set the regrab flag (SIGCONT, after suspend/resume).
    pub fn on_sigcont(mut self, sig: libc::c_int) -> Self {
        self.handlers.push(Handler::Regrab(sig));
        self
    }

    /// Install every configured handler, reporting the ones that did not take.
    ///
    /// # SIGCHLD
    ///
    /// `SIGCHLD` is always installed with `SA_NOCLDWAIT | SA_RESTART` and
    /// `SIG_DFL`, and that pairing is the whole point: `SA_NOCLDWAIT` tells the
    /// kernel to discard a child's exit status instead of leaving the child as a
    /// zombie, which is what a window manager wants for the clients it starts.
    /// It is not "the window manager reaps" — nothing here waits on anything,
    /// and no child of the process is ever waited for. The kernel reaps them
    /// before `waitpid` could, and the observable consequence is that
    /// `waitpid`, `Child::wait` and `Command::output` all report `ECHILD` for
    /// *any* child of a process that has installed this.
    ///
    /// So `SA_NOCLDWAIT` is correct here and explicit reaping would not be: a
    /// reaper would be machinery nothing calls, and it would race any future
    /// caller that did need a status. What it costs is that code in the same
    /// process may not depend on a child's exit status, which is a constraint
    /// on callers rather than something `install` can enforce. The
    /// `SA_NOCLDWAIT` flag itself is a `sigaction` flag, not a disposition: it
    /// is inherited across `fork` but *not* across `exec`, so it never leaks
    /// into a child that execs.
    ///
    /// A failure here is reported rather than swallowed for the same reason the
    /// handler failures are: without `SA_NOCLDWAIT` every autostarted client
    /// becomes a zombie for the life of the process.
    ///
    /// # Partial installation
    ///
    /// Install is not atomic and does not pretend to be. The dispositions it
    /// overwrites are never read back, so there is nothing to restore, and a
    /// refusal part-way through leaves the earlier installs in place. The
    /// returned vector is the complete account of what is missing, in the order
    /// the signals were configured so `SIGCHLD` is always first. Deciding
    /// whether a given refusal is fatal belongs to the caller, which is the
    /// only party that knows what it was going to depend on.
    ///
    /// A single call that ignores the result is a programming error, not a
    /// style choice: the caller would be claiming a signal-controlled lifecycle
    /// with dispositions it never installed.
    pub fn install(self) -> Vec<libc::c_int> {
        let mut failed = Vec::new();
        if !install_raw(
            libc::SIGCHLD,
            libc::SIG_DFL,
            libc::SA_NOCLDWAIT | libc::SA_RESTART,
        ) {
            failed.push(libc::SIGCHLD);
        }

        for sig in &self.ignored {
            if !install_raw(*sig, libc::SIG_IGN, libc::SA_RESTART) {
                failed.push(*sig);
            }
        }
        for h in &self.handlers {
            let (sig, ok) = match h {
                Handler::Term(sig) => (*sig, install_term(*sig)),
                Handler::Regrab(sig) => (*sig, install_regrab(*sig)),
            };
            if !ok {
                failed.push(sig);
            }
        }
        failed
    }
}

impl Default for Signal {
    fn default() -> Self {
        Self::new()
    }
}

/// `extern "C"` trampoline that flips the quit flag. Lives for the whole
/// process; safe because it only touches an `AtomicBool`.
extern "C" fn term_trampoline(_: libc::c_int) {
    QUIT_REQUESTED.store(true, ORD);
}

extern "C" fn regrab_trampoline(_: libc::c_int) {
    NEED_REGRAB.store(true, ORD);
}

fn install_term(sig: libc::c_int) -> bool {
    install_raw(sig, term_trampoline as *const () as usize, libc::SA_RESTART)
}

fn install_regrab(sig: libc::c_int) -> bool {
    install_raw(
        sig,
        regrab_trampoline as *const () as usize,
        libc::SA_RESTART,
    )
}

/// Install a disposition for `sig`.
///
/// The `action` is a `sighandler_t`: one of `SIG_DFL`/`SIG_IGN`, or a plain
/// `extern "C" fn(c_int)` cast to a pointer, which is what the kernel stores
/// in the `sa_sigaction` union member when `SA_SIGINFO` is *not* set. Every
/// caller here is one-argument and never sets `SA_SIGINFO`, so writing the
/// handler through that member is correct; a caller that added `SA_SIGINFO`
/// would need the three-argument `sa_sigaction` member instead.
///
/// This is the crate's only disposition primitive. `SIG_DFL`, `SIG_IGN` and a
/// handler therefore share one safety argument rather than three near-copies
/// of it, and `SA_NOCLDWAIT` is just another `flags` value here.
///
/// Returns `false` instead of panicking: a `sigaction` refused by a seccomp
/// policy or a bad signal number must not take down a window manager from
/// inside a library. The caller reports it.
fn install_raw(sig: libc::c_int, action: usize, flags: libc::c_int) -> bool {
    // SAFETY: `sa` is a fully initialised `sigaction` before the call — zeroed,
    // with `action` written to the handler member, `flags` to `sa_flags` and an
    // explicitly emptied `sa_mask`. Passing a null `oldact` is always valid and
    // is what lets this not have to save the disposition it is about to
    // replace. `action` is either a `SIG_DFL`/`SIG_IGN` constant or the address
    // of an `extern "C" fn(c_int)` that is linked for the life of the process,
    // so the kernel cannot be left holding a dangling handler. The only failure
    // the call can report is a refusal, which is returned to the caller.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = action;
        sa.sa_flags = flags;
        libc::sigemptyset(&mut sa.sa_mask);
        if libc::sigaction(sig, &sa, std::ptr::null_mut()) != 0 {
            // Reported by the caller rather than printed here: a library must
            // not decide on its own how loudly a window manager complains.
            return false;
        }
    }
    true
}

/// Detach from the launching terminal so the WM outlives the shell that
/// started it (standard daemon/WM behavior). Failures are non-fatal: this is
/// best-effort detach, and it returns nothing.
///
/// `setsid` is deliberately *not* called. Under `startx` the WM is a child of
/// the very login session that owns the VT/seat Xorg runs on, and forcing a
/// fresh POSIX session here has been observed correlating with Xorg losing
/// its DRM master mid-startup (`EnterVT failed`, `Failed to enable any CRTC`)
/// during Maverick's autostart phase. Staying in the launching session keeps
/// seat/session assignment, which Xorg depends on, untouched. The stdio
/// redirect below is retained so a display-manager-less `startx` launch does
/// not leave the starting shell blocked on the WM.
pub fn detach_from_terminal() {
    unsafe {
        // Already detached (e.g. launched by a display manager, or stdin is
        // not a real terminal)? Nothing to do.
        if libc::isatty(libc::STDIN_FILENO) == 0 {
            return;
        }

        let devnull = match std::ffi::CString::new("/dev/null") {
            Ok(s) => s,
            Err(_) => return,
        };
        let fd = libc::open(devnull.as_ptr(), libc::O_RDWR);
        if fd < 0 {
            return;
        }
        libc::dup2(fd, libc::STDIN_FILENO);
        libc::dup2(fd, libc::STDOUT_FILENO);
        // stderr left open so log messages reach journald / the terminal.
        if fd > 2 {
            libc::close(fd);
        }
    }
}

/// Wait until one of the X11/control wake descriptors is readable. `None`
/// blocks until an event (or EINTR) instead of imposing a heartbeat poll.
///
/// The window manager's event loop blocks on the X11 connection socket *and* the
/// control hub's self-pipe, so this takes the set rather than a single
/// descriptor. Keeping the `poll(2)` FFI here means the WM crate stays
/// `unsafe`-free.
///
/// Takes raw descriptors so the WM crate stays `unsafe`-free; the borrow is
/// taken here, under the one safety argument that has to exist either way.
pub fn wait_readable_fds(
    fds: &[std::os::unix::io::RawFd],
    timeout: Option<std::time::Duration>,
) -> bool {
    if fds.is_empty() {
        return true;
    }
    let borrowed: Vec<std::os::unix::io::BorrowedFd<'_>> = fds
        .iter()
        .copied()
        // SAFETY: every descriptor here is one the caller is already holding
        // open and passes in solely to be waited on, and the borrow does not
        // outlive this call.
        .map(|fd| unsafe { std::os::unix::io::BorrowedFd::borrow_raw(fd) })
        .collect();
    let mut pfds: Vec<rustix::event::PollFd<'_>> = borrowed
        .iter()
        .map(|fd| rustix::event::PollFd::new(fd, rustix::event::PollFlags::IN))
        .collect();
    let ts = timeout.map(|v| {
        // `tv_nsec` is a remainder, not a total: `poll(2)` reads the pair as
        // one value and a `tv_nsec` of 2e9 is undefined rather than 2 seconds.
        let total = v.as_secs().min(i64::MAX as u64);
        let nanos = v.subsec_nanos();
        rustix::event::Timespec {
            tv_sec: total as i64,
            tv_nsec: nanos as i64,
        }
    });
    let r = rustix::event::poll(&mut pfds, ts.as_ref());
    match r {
        Ok(0) => false,
        Ok(_) => pfds
            .iter()
            .any(|p| p.revents().contains(rustix::event::PollFlags::IN)),
        // Any error is a wakeup, including EINTR: the caller re-checks its
        // flags either way, and a busy loop here would spin the WM.
        Err(_) => true,
    }
}

pub mod control;
pub mod ctl;
pub mod discover;
pub mod hub;
pub mod identity;
pub mod json;
pub mod session;

pub use control::ControlServer;
pub use hub::{ControlCommand, ControlHub};
pub use identity::{self_info, InstanceInfo, DEFAULT_NAME};
pub use session::{Session, SessionName, SessionState};

/// Shared pieces for the property tests that are compiled into the library
/// because the functions they cover are private.
#[cfg(test)]
pub(crate) mod prop_support {
    use proptest::prelude::*;
    use proptest::string::string_regex;

    /// Where a failing property persists its counterexample.
    ///
    /// `proptest!` always stamps the config with `file!()`, so a property
    /// compiled into the library would drop its regression file under the crate
    /// root next to the module it covers. The persistence root is redirected
    /// into `tests/` instead, keeping every counterexample in this crate under
    /// the same tree as the integration property suites.
    pub fn config() -> proptest::test_runner::Config {
        proptest::test_runner::Config {
            failure_persistence: Some(Box::new(
                proptest::test_runner::FileFailurePersistence::SourceParallel(
                    "tests/proptest-regressions",
                ),
            )),
            ..proptest::test_runner::Config::default()
        }
    }

    /// Free-form text a user, a client or a window can put into a field:
    /// instance names, window titles, executable paths, CLI words. Quotes,
    /// backslashes, separators, control bytes and non-ASCII are
    /// over-represented, because those are the characters that decide whether a
    /// payload survives the JSON escaper, the line framing of the control
    /// protocol and the hand-rolled ficha reader.
    pub fn text() -> impl Strategy<Value = String> {
        prop_oneof![
            3 => proptest::collection::vec(
                    prop_oneof![
                        any::<char>(),
                        Just('"'),
                        Just('\\'),
                        Just('/'),
                        Just('\n'),
                        Just('\r'),
                        Just('\t'),
                        Just('\u{0000}'),
                        Just('\u{000c}'),
                        Just('\u{001f}'),
                        Just('\u{007f}'),
                        Just('\u{00e9}'),
                        Just('\u{1f600}'),
                    ],
                    0..24,
                )
                .prop_map(|cs| cs.into_iter().collect()),
            2 => string_regex("[\"\\\\,{}: \x00-\x1f]{0,16}").expect("static pattern"),
            1 => string_regex("[^\x00-\x7f]{0,12}").expect("static pattern"),
            1 => string_regex(".").expect("static pattern"),
        ]
    }
}
