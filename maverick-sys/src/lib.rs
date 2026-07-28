// maverick-sys/src/lib.rs
// Safe (well, safer) wrappers around the bits of libc Maverick needs at
// startup, plus instance identity and a Unix-socket control channel so an
// external tool can discover and close Maverick instances (even several on
// different TTYs/DISPLAYs). The only `unsafe` in the whole project lives
// here, isolated in one small crate. Everything the rest of the codebase
// touches is a plain safe function or an AtomicBool.

use std::sync::atomic::{AtomicBool, Ordering};

/// Ordering used for the flag hand-offs between signal handler and event loop.
/// SeqCst keeps it simple and correct; these are rare, low-contention writes.
const ORD: Ordering = Ordering::SeqCst;

// ─── Cross-thread/signal flags ───────────────────────────────────────────────
//
// Formerly `static`s living in `backend/x11.rs`. Now owned by this crate so the
// WM core has no `unsafe` and no raw statics. The event loop polls these.

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

// ─── Signal builder ──────────────────────────────────────────────────────────

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
        // SIGCHLD: reap children, never become a zombie parent.
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
fn install_raw_fn(func: extern "C" fn(libc::c_int), sig: libc::c_int, flags: libc::c_int) {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = func as *const () as usize;
        sa.sa_flags = flags;
        libc::sigemptyset(&mut sa.sa_mask);
        if libc::sigaction(sig, &sa, std::ptr::null_mut()) != 0 {
            panic!("sigaction({sig}) failed");
        }
    }
}

/// Install a handler from a `sighandler_t` constant (SIG_DFL / SIG_IGN).
fn install_raw(sig: libc::c_int, action: usize, flags: libc::c_int) {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = action;
        sa.sa_flags = flags;
        libc::sigemptyset(&mut sa.sa_mask);
        if libc::sigaction(sig, &sa, std::ptr::null_mut()) != 0 {
            panic!("sigaction({sig}) failed");
        }
    }
}

// ─── Terminal detachment ─────────────────────────────────────────────────────

/// Detach from the launching terminal so the WM outlives the shell that
/// started it (standard daemon/WM behavior). Returns nothing; failures are
/// non-fatal (best-effort detach).
pub fn detach_from_terminal() {
    unsafe {
        // New session/process group with no controlling terminal.
        // Ignore failure (EPERM if already session leader) — isatty will tell us.
        let _ = libc::setsid();

        // Already detached (e.g. launched by a display manager)?
        if libc::isatty(libc::STDIN_FILENO) == 0 {
            return;
        }

        // Redirect stdin/stdout to /dev/null so we don't hang the terminal.
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

// ─── Event-loop poll helper ──────────────────────────────────────────────────

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
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
    // SAFETY: `pfd` is a valid, initialized pollfd for the duration of the call.
    let r = unsafe { libc::poll(&mut pfd, 1, ms) };
    match r {
        0 => false, // timeout, nothing readable
        n if n > 0 => {
            // Only claim readable if POLLIN is set; POLLERR/POLLHUP/POLLNVAL
            // should wake the caller so it can react to the error.
            pfd.revents & libc::POLLIN != 0
        }
        _ => true, // error/EINTR: wake and let the caller re-check
    }
}

// ─── Modules ─────────────────────────────────────────────────────────────────

pub mod control;
pub mod discover;
pub mod hub;
pub mod identity;

// Re-export the most common items at the crate root for convenience.
pub use control::ControlServer;
pub use hub::{ControlCommand, ControlHub};
pub use identity::{self_info, InstanceInfo, DEFAULT_NAME};
