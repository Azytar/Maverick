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
//! What is not owned: the X11 connection fd passed to [`wait_readable`], the
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
//! and installs `SIGCHLD` (`SA_NOCLDWAIT|SA_RESTART`) plus the configured
//! handlers. Static flags (`QUIT_REQUESTED`, `NEED_REGRAB`) are written by
//! `extern "C"` trampolines and read by the WM thread via
//! [`quit_requested`]/[`need_regrab`]. [`ControlServer`] owns the listener and
//! a `stop` flag; [`ControlHub`] is shared via `Arc` between server and WM
//! threads. `detach_from_terminal` is called once at startup before the X
//! connection is opened; [`wait_readable`] is called each event-loop iteration.
//!
//! # Safety
//!
//! `sigaction` installs use `zeroed` + `sigemptyset` and `SA_RESTART`; only
//! `AtomicBool::store` with `SeqCst` runs inside handlers. `poll` wraps a
//! valid `pollfd` and treats `EINTR`/errors as wakeups. `detach_from_terminal`
//! is best-effort, never calls `setsid`, and only redirects stdin/stdout to
//! `/dev/null` when `isatty(STDIN)` is true. The remaining `unsafe` in this
//! crate is confined to `getuid`/`getgid` identity reads and to the
//! [`session`] process-tree signals, which gate every `kill`/`killpg` on a
//! recorded process start time so a recycled PID cannot be hit.

use std::sync::atomic::{AtomicBool, Ordering};

/// Ordering used for the flag hand-offs between signal handler and event loop.
/// SeqCst keeps it simple and correct; these are rare, low-contention writes.
const ORD: Ordering = Ordering::SeqCst;

static QUIT_REQUESTED: AtomicBool = AtomicBool::new(false);
static NEED_REGRAB: AtomicBool = AtomicBool::new(false);

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

/// Request the WM to quit (used by the control socket's `quit` command).
/// The main loop polls `quit_requested()` and tears down.
#[inline]
pub fn request_quit() {
    QUIT_REQUESTED.store(true, ORD);
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

    /// On this signal, set the quit flag (SIGTERM).
    pub fn on_sigterm(mut self, sig: libc::c_int) -> Self {
        self.handlers.push(Handler::Term(sig));
        self
    }

    /// On this signal, set the regrab flag (SIGCONT, after suspend/resume).
    pub fn on_sigcont(mut self, sig: libc::c_int) -> Self {
        self.handlers.push(Handler::Regrab(sig));
        self
    }

    /// Install every configured handler.
    ///
    /// SIGCHLD is always installed with `SA_NOCLDWAIT | SA_RESTART` so the WM
    /// reaps spawned children (alacritty, rofi, …) without leaving zombies —
    /// that behavior is mandatory for a WM, not optional.
    pub fn install(self) {
        install_raw(
            libc::SIGCHLD,
            libc::SIG_DFL,
            libc::SA_NOCLDWAIT | libc::SA_RESTART,
        );

        for sig in &self.ignored {
            install_raw(*sig, libc::SIG_IGN, libc::SA_RESTART);
        }
        for h in &self.handlers {
            match h {
                Handler::Term(sig) => install_term(*sig),
                Handler::Regrab(sig) => install_regrab(*sig),
            }
        }
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

fn install_term(sig: libc::c_int) {
    install_raw_fn(term_trampoline, sig, libc::SA_RESTART);
}

fn install_regrab(sig: libc::c_int) {
    install_raw_fn(regrab_trampoline, sig, libc::SA_RESTART);
}

/// Install a handler whose address is a plain `extern "C" fn` (no captured
/// state) — safe to pass straight to `sigaction`.
/// Returns `false` instead of panicking: a transient `sigaction` failure
/// (seccomp, bad sig) must not take down the WM from inside a library.
fn install_raw_fn(func: extern "C" fn(libc::c_int), sig: libc::c_int, flags: libc::c_int) -> bool {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        // NOTE: without SA_SIGINFO the kernel uses the `sa_handler` union
        // member (1-arg handler), which shares storage with `sa_sigaction`.
        // Our trampolines are 1-arg `extern "C" fn(c_int)`, so this assignment
        // is correct as long as callers never add SA_SIGINFO.
        sa.sa_sigaction = func as *const () as usize;
        sa.sa_flags = flags;
        libc::sigemptyset(&mut sa.sa_mask);
        if libc::sigaction(sig, &sa, std::ptr::null_mut()) != 0 {
            eprintln!("maverick-sys: sigaction({sig}) failed; continuing");
            return false;
        }
    }
    true
}

/// Install a handler from a `sighandler_t` constant (SIG_DFL / SIG_IGN).
fn install_raw(sig: libc::c_int, action: usize, flags: libc::c_int) -> bool {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = action;
        sa.sa_flags = flags;
        libc::sigemptyset(&mut sa.sa_mask);
        if libc::sigaction(sig, &sa, std::ptr::null_mut()) != 0 {
            eprintln!("maverick-sys: sigaction({sig}) failed; continuing");
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

/// Wait until `fd` is readable or `timeout` elapses, whichever comes first.
///
/// The WM's event loop uses this to block on the X11 connection socket while
/// still waking up periodically to drain control-socket commands (from
/// `ControlHub`). Keeping the `poll(2)` FFI here means the WM crate stays
/// `unsafe`-free.
///
/// Returns `true` if the fd became readable, `false` on timeout. Errors
/// (including `EINTR`) are treated as "wake up and let the caller re-check",
/// i.e. they return `true` so the loop makes progress.
pub fn wait_readable(fd: std::os::unix::io::RawFd, timeout: std::time::Duration) -> bool {
    wait_readable_fds(&[fd], Some(timeout))
}

/// Wait until one of the X11/control wake descriptors is readable. `None`
/// blocks until an event (or EINTR) instead of imposing a heartbeat poll.
pub fn wait_readable_fds(
    fds: &[std::os::unix::io::RawFd],
    timeout: Option<std::time::Duration>,
) -> bool {
    if fds.is_empty() {
        return true;
    }
    let mut pfds: Vec<libc::pollfd> = fds
        .iter()
        .copied()
        .map(|fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    let ms = timeout
        .map(|v| v.as_millis().min(i32::MAX as u128) as libc::c_int)
        .unwrap_or(-1);
    // SAFETY: every pollfd points at a caller-owned descriptor and remains
    // valid for the duration of the call.
    let r = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, ms) };
    match r {
        0 => false,
        n if n > 0 => pfds.iter().any(|p| p.revents & libc::POLLIN != 0),
        _ => true,
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
