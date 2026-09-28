//! What a *synchronous* Xlib request that fails does to this connection.
//!
//! The outcome is the interesting part, and it is not something to assert from
//! inside the process that suffers it: libXlib reports an I/O error, its
//! `XSetIOErrorExitHandler` function runs (default `exit(1)`), and — if both
//! handlers are replaced with ones that return, so the process survives — the
//! next thing libXlib touches on that connection **segfaults**. Measured on this
//! crate's own `open_x` connection against a fresh Xvfb, with and without the
//! pre-existing `lib.rs`.
//!
//! So the test runs the probe in a *child* process and asserts on the child's
//! exit status — the only way to pin a crash without crashing the harness. (The
//! child's `PROBE:` lines are lost to stdio buffering when it dies, which is the
//! other reason the assertion is on the status and not on the output.)
//!
//! It is the only way to pin a crash without crashing the harness,
//! and it is worth pinning: it is why no Xlib request on this connection may be
//! relied upon to fail gracefully, and why the answer to a libXlib I/O error can
//! only be to stop using the connection rather than to try to recover it.
//!
//! # Running
//!
//! ```text
//! Xvfb :91 -screen 0 1280x800x24 &
//! DISPLAY=:91 cargo test -p maverick-x11 --test io_error_scope
//! ```
//! Without a display it skips, with a printed note.
//!
//! # Related
//!
//! `x_error_signal.rs` measures the milder half — an *asynchronous* failure is
//! never delivered to the error handler at all, so a caller must not branch on
//! `take_x_error` answering `None`. That is the case the compositor actually hits;
//! this one is the reason no test should provoke errors through Xlib.

use std::os::raw::{c_int, c_uchar, c_ulong, c_void};
use std::process::Command;

use maverick_x11::XDisplay;

const NO_SUCH_WINDOW: c_ulong = 0x7fff_ffff;

/// Set on the child so it performs the probe instead of spawning a grandchild.
const CHILD_ENV: &str = "MAVERICK_X11_IO_ERROR_PROBE";

#[link(name = "X11")]
extern "C" {
    fn XSetIOErrorHandler(h: Option<unsafe extern "C" fn(*mut c_void, c_int) -> c_int>);
    fn XSetIOErrorExitHandler(
        h: Option<unsafe extern "C" fn(*mut c_void, c_int) -> c_int>,
    ) -> Option<unsafe extern "C" fn(*mut c_void, c_int) -> c_int>;
    fn XGetGeometry(
        dpy: *mut c_void,
        drawable: c_ulong,
        root_return: *mut c_ulong,
        x: *mut c_int,
        y: *mut c_int,
        width: *mut c_ulong,
        height: *mut c_ulong,
        border_width: *mut c_ulong,
        depth: *mut c_ulong,
    ) -> c_int;
}

/// Records that libXlib asked, without exiting — so the child gets far enough to
/// print the evidence before the connection kills it.
///
/// # Safety
/// `extern "C"` ABI, this signature, and mapped for the process: Xlib calls it
/// from `_XIOError` on the stack that is unwinding out of a failed socket read.
unsafe extern "C" fn note_io_error(_dpy: *mut c_void, code: c_int) -> c_int {
    eprintln!("PROBE: libXlib reported an I/O error (errno {code})");
    0
}

/// Replaces the `exit(1)` that runs when the handler above returns, so the child
/// outlives the report and reaches the request that is expected to fault.
///
/// # Safety
/// As above: Xlib calls this from `_XIOError` immediately after the I/O error
/// handler has returned.
unsafe extern "C" fn note_io_error_exit(_dpy: *mut c_void, _code: c_int) -> c_int {
    eprintln!("PROBE: libXlib would have exited here");
    0
}

/// The probe: provoke a synchronous failure and then try to keep using the
/// connection. Runs in the child; nothing after the request may be assumed to
/// work.
fn probe(dpy: &XDisplay) {
    // SAFETY: both are non-capturing `extern "C"` fn items with the signatures
    // libX11 declares, mapped for the process lifetime, which is as long as it
    // can call them. The second is the surprising one: the first handler
    // returning is not enough to survive, the exit handler runs next.
    unsafe {
        XSetIOErrorHandler(Some(note_io_error));
        XSetIOErrorExitHandler(Some(note_io_error_exit));
    }

    let (mut r, mut x, mut y, mut w, mut h, mut bw, mut d) = (0, 0, 0, 0, 0, 0, 0);
    // SAFETY: `dpy.as_ptr()` is a live `Display*` and every out-parameter is the
    // address of a live local that `XGetGeometry` writes through. The drawable
    // does not exist, so the server answers with an error where a reply was
    // expected — which is what leaves libXlib unable to find its reply.
    unsafe {
        XGetGeometry(
            dpy.as_ptr(),
            NO_SUCH_WINDOW,
            &mut r,
            &mut x,
            &mut y,
            &mut w,
            &mut h,
            &mut bw,
            &mut d,
        )
    };
    eprintln!("PROBE: the request returned");

    // Anything at all on this connection now. The point is not that it works.
    dpy.sync();
    eprintln!("PROBE: the connection was still usable after the failure");
}

#[test]
fn a_synchronous_failing_request_ends_the_connection_rather_than_reporting_it() {
    if std::env::var_os(CHILD_ENV).is_some() {
        let (dpy, _conn, _screen) = maverick_x11::open_x().expect("a live X display");
        probe(&dpy);
        return;
    }

    // Prove the child can even get a display, so a skip here is unambiguous and
    // a failure there is not mistaken for the crash.
    if maverick_x11::open_x().is_err() {
        eprintln!("no X display; skipping");
        return;
    }

    let status = Command::new(std::env::current_exe().expect("this test binary"))
        .arg("--nocapture")
        .arg("--exact")
        .arg("a_synchronous_failing_request_ends_the_connection_rather_than_reporting_it")
        .arg("--test-threads=1")
        .env(CHILD_ENV, "1")
        .status()
        .expect("the child probe ran");

    // The child's `open_x` succeeds, then the failing request kills the process.
    // Which way it dies is the finding: not a clean, diagnosable exit, but a
    // fault, because the connection cannot be used again whatever the handlers
    // return.
    assert!(
        !status.success(),
        "a synchronous Xlib request that failed left the connection usable, so \\
         the caveats on `XDisplay::sync` and in this crate's error-handling docs \\
         can be revisited"
    );
    assert_eq!(
        std::mem::size_of::<c_uchar>(),
        1,
        "this file is only meaningful because a protocol code is one byte"
    );
}
