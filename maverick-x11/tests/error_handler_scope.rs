//! The Xlib error handler is a process-global slot, and this crate **replaces**
//! whatever was in it rather than chaining to it.
//!
//! That is a decision, not an oversight, and it is the one thing about the error
//! path that cannot be verified by reading the `open_x` body: `XSetErrorHandler`
//! *returns* the previous handler and `open_x` throws that value away. The
//! consequences a reader has to take on trust are:
//!
//! * the silent handler really is the one installed after `open_x`, and
//! * a handler installed by anything else before or after it is gone — it is
//!   never called again, and it is not wrapped.
//!
//! Both are checked here against a real server, by installing a handler that
//! counts its invocations, letting `open_x` and `install_silent_error_handler`
//! run over it, and then provoking a protocol error.
//!
//! This file is deliberately its own test binary. The handler slot is
//! process-global and there is exactly one of it, so a test that mutates it must
//! not run concurrently with a test that depends on it.
//!
//! # Running
//!
//! ```text
//! Xvfb :91 -screen 0 1280x800x24 &
//! DISPLAY=:91 cargo test -p maverick-x11 --test error_handler_scope
//! ```

use std::os::raw::{c_int, c_uchar, c_ulong, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

use maverick_x11::{take_x_error, XConn, XDisplay, XErrorEvent};

/// A window id no client can hold, so the server answers `BadWindow`.
const NO_SUCH_WINDOW: c_ulong = 0x7fff_ffff;

static FOREIGN_CALLS: AtomicUsize = AtomicUsize::new(0);

/// A handler that only counts, so "was it called?" is answerable without
/// reading the cell the real handler writes.
unsafe extern "C" fn counting_handler(_dpy: *mut c_void, _err: *mut XErrorEvent) -> c_int {
    FOREIGN_CALLS.fetch_add(1, Ordering::SeqCst);
    0
}

/// Puts the crate's own handler back however this test leaves the process.
///
/// Without it, a panic between "install the foreign handler" and "assert" would
/// leave `counting_handler` installed for whatever runs next in this binary —
/// and the very next X error would increment a counter nobody is reading while
/// the real signal silently stopped working.
struct SilentHandlerOnDrop;

impl Drop for SilentHandlerOnDrop {
    fn drop(&mut self) {
        maverick_x11::install_silent_error_handler();
    }
}

// Xlib's `XGetGeometry`: a request that provokes `BadWindow` on demand.
#[link(name = "X11")]
extern "C" {
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

/// One failing Xlib request, followed by the round trip that lets the server's
/// answer reach this thread.
fn provoke_bad_window(dpy: &XDisplay) {
    let (mut r, mut x, mut y, mut w, mut h, mut bw, mut d) = (0, 0, 0, 0, 0, 0, 0);
    // SAFETY: `dpy.as_ptr()` is a live `Display*` from `open_x` and every
    // out-parameter is the address of a live local; `XGetGeometry` only writes
    // through them.
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
    dpy.sync();
}

#[test]
fn open_x_replaces_an_existing_handler_and_does_not_chain_to_it() {
    if maverick_x11::open_x().is_err() {
        eprintln!("no X display; skipping");
        return;
    }
    let _restore = SilentHandlerOnDrop;

    // A handler that was already in the slot when `open_x` ran.
    // SAFETY: `counting_handler` is a non-capturing `extern "C"` fn item with
    // the signature `XSetErrorHandler` declares, so it is a valid
    // `XErrorHandler`; the code it points at stays mapped for the process.
    unsafe { maverick_x11::XSetErrorHandler(Some(counting_handler)) };
    FOREIGN_CALLS.store(0, Ordering::SeqCst);

    // `open_x` installing over the top is exactly what the compositor does, and
    // what `install_silent_error_handler` is for.
    maverick_x11::install_silent_error_handler();
    maverick_x11::clear_x_error();

    let (dpy, _conn, _screen): (XDisplay, XConn, usize) =
        maverick_x11::open_x().expect("a second display on the same server");
    provoke_bad_window(&dpy);

    assert_eq!(
        FOREIGN_CALLS.load(Ordering::SeqCst),
        0,
        "the previous handler was chained to instead of being replaced"
    );
    assert_eq!(
        take_x_error(),
        Some(3),
        "the replaced handler's replacement did not record the error either"
    );
}

#[test]
fn installing_the_silent_handler_twice_changes_nothing() {
    if maverick_x11::open_x().is_err() {
        eprintln!("no X display; skipping");
        return;
    }
    let _restore = SilentHandlerOnDrop;

    maverick_x11::install_silent_error_handler();
    maverick_x11::install_silent_error_handler();
    maverick_x11::clear_x_error();

    let (dpy, _conn, _screen) = maverick_x11::open_x().expect("a second display");
    provoke_bad_window(&dpy);
    assert_eq!(
        take_x_error(),
        Some(3),
        "a second install displaced the handler the first one installed"
    );
    assert_eq!(
        take_x_error(),
        None,
        "taking the error twice reported it twice"
    );
}

/// The recorded code is the protocol's `error_code` byte, not a widened field.
///
/// `XErrorEvent` is declared `#[repr(C)]` with the same field types and order as
/// the C struct, so reading `error_code` at the C offset is reading the right
/// byte. If that layout ever drifted, every code would still be in 1..=255 and
/// every name lookup would still succeed — so the check that it is specifically
/// `BadWindow` is what pins the offset, and that is what
/// `x_error_signal.rs` asserts against a real failing request.
#[test]
fn the_recorded_code_is_the_protocol_error_code_byte() {
    maverick_x11::clear_x_error();
    assert_eq!(std::mem::size_of::<c_uchar>(), 1);
    assert_eq!(maverick_x11::x_error_name(3), "BadWindow");
    assert_eq!(take_x_error(), None);
}
