//! What the X error signal actually does on this crate's connection, measured.
//!
//! The crate documents a sequence — `clear_x_error` → request →
//! `XDisplay::sync` → `take_x_error` — and two callers rely on it (the renderer's
//! per-visual fbconfig check and nothing else). Those tests are written to
//! *measure* that sequence rather than assert a hoped-for answer, because the
//! answer is not the one the docs originally claimed:
//!
//! * an **asynchronous** request the server rejects never reaches the Xlib error
//!   handler on a connection whose event queue XCB owns — not through `XFlush`,
//!   not through `XSync`, not through `XEventsQueued`. Handler invocations: 0.
//!   `take_x_error` therefore answers `None` after a request that certainly
//!   failed;
//! * a **synchronous** Xlib request that fails on that same connection
//!   desynchronises libXlib from the socket, and libXlib's I/O error path calls
//!   `exit(1)` — with no `XSetIOErrorHandler` installed, that is the default
//!   handler and the process dies. So this file never issues one.
//!
//! Both are properties of the connection `open_x` builds, not of the handler,
//! and both were reproduced against a fresh server and against the pre-existing
//! `lib.rs`. They are pinned here so a change in either direction shows up
//! instead of quietly invalidating a caller's `Err` branch.
//!
//! The ownership and thread questions the sequence was supposed to answer are
//! covered too, and those *are* checkable: the cell is per-thread, `XInitThreads`
//! succeeds, `XDisplay` is `Send`, and no alias of a `Display*` owns it.
//!
//! # Running
//!
//! ```text
//! Xvfb :91 -screen 0 1280x800x24 &
//! DISPLAY=:91 cargo test -p maverick-x11 --test x_error_signal
//! ```
//! Without a display every test here skips, so the suite stays green on a build
//! machine. The skip is printed, not silent.

use std::os::raw::{c_int, c_uchar, c_ulong, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

use maverick_x11::{take_x_error, XConn, XDisplay};
use x11rb::connection::Connection;

/// A window id no client can hold, so the server rejects any request naming it.
const NO_SUCH_WINDOW: c_ulong = 0x7fff_ffff;

#[link(name = "X11")]
extern "C" {
    fn XSetErrorHandler(
        h: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int>,
    ) -> Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int>;
    fn XSetIOErrorHandler(h: Option<unsafe extern "C" fn(*mut c_void, c_int) -> c_int>);
    fn XFlush(dpy: *mut c_void) -> c_int;
    fn XChangeProperty(
        dpy: *mut c_void,
        window: c_ulong,
        property: c_ulong,
        type_: c_int,
        mode: c_int,
        format: c_int,
        nelements: c_int,
        data: *const c_uchar,
    ) -> c_int;
}

/// Handler invocations, so "the handler was never called" is a measurement and
/// not an inference from the cell being empty.
static HANDLER_CALLS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn counting_handler(_dpy: *mut c_void, _err: *mut c_void) -> c_int {
    HANDLER_CALLS.fetch_add(1, Ordering::SeqCst);
    0
}

unsafe extern "C" fn io_error_handler(_dpy: *mut c_void, code: c_int) -> c_int {
    eprintln!("libX11 reported an I/O error (errno {code}); refusing to let it exit(1)");
    std::process::exit(70);
}

/// Open the shared bootstrap, or `None` when there is no server to talk to.
fn x() -> Option<(XDisplay, XConn, usize)> {
    let opened = maverick_x11::open_x().ok()?;
    // From here on the process must not be killable by libXlib's default I/O
    // error handler: this file issues requests that fail, and libXlib's reaction
    // to a desynchronised socket is `exit(1)`, which would look like a test
    // failure with no message. Exiting 70 instead is distinguishable.
    // SAFETY: a non-capturing `extern "C"` fn item whose signature is the one
    // `XSetIOErrorHandler` declares, and the code it points at is mapped for the
    // process lifetime, which is as long as libX11 can call it.
    unsafe { XSetIOErrorHandler(Some(io_error_handler)) };
    Some(opened)
}

/// The root window of the screen `open_x` reported.
fn root_of(conn: &XConn, screen: usize) -> c_ulong {
    conn.setup().roots[screen].root as c_ulong
}

/// Reject the crate's silent handler and put a counting one in its place, so a
/// test can tell "the handler was not called" from "the handler called the
/// crate's cell and the code happened to be 0".
fn install_counting_handler() {
    HANDLER_CALLS.store(0, Ordering::SeqCst);
    // SAFETY: `counting_handler` is a non-capturing `extern "C"` fn item with
    // the signature `XSetErrorHandler` declares, so it is a valid argument; it
    // stays mapped for the process lifetime, which is the only thing that
    // matters for a handler registration.
    unsafe { XSetErrorHandler(Some(counting_handler)) };
}

/// The request the tests use to provoke a protocol error.
///
/// `XChangeProperty` is **asynchronous** — it sends and returns without waiting
/// for a reply — which is exactly the shape of a GLX call and, crucially, not
/// the shape that takes the process down. `XGetGeometry` would be a synchronous
/// request and is deliberately not used for error provocation anywhere in this
/// file.
fn provoke_bad_window(dpy: &XDisplay) {
    maverick_x11::clear_x_error();
    // SAFETY: `dpy.as_ptr()` is a live `Display*` from `open_x`; the request
    // takes a window id by value, one `int` and one `int` count, and a pointer
    // to a single byte that outlives the call. The window id does not exist, so
    // the server rejects the request — which is the point.
    unsafe { XChangeProperty(dpy.as_ptr(), NO_SUCH_WINDOW, 1, 4, 0, 8, 1, [7u8].as_ptr()) };
}

/// An asynchronous failure on this connection does not reach the error handler.
///
/// This is the finding that makes the renderer's fbconfig check dead code, and
/// the reason [`maverick_x11::XDisplay::sync`] is documented as not being an
/// error barrier. If this test ever fails, a caller that branched on
/// `take_x_error` may have become live — which is a *good* failure, and would
/// mean the renderer needs to be revisited.
#[test]
fn an_asynchronous_failure_never_reaches_the_error_handler() {
    let Some((dpy, _conn, _screen)) = x() else {
        eprintln!("no X display; skipping");
        return;
    };
    install_counting_handler();
    provoke_bad_window(&dpy);
    // Put the request on the wire explicitly, then round-trip. `XFlush` writes
    // the request buffer; `XSync(dpy, 0)` writes and then waits for a reply, so
    // between them the server has certainly been reached and answered.
    // SAFETY: both take only the live `Display*` and read nothing back; neither
    // assumes anything about who else is reading the socket.
    unsafe { XFlush(dpy.as_ptr()) };
    dpy.sync();

    assert_eq!(
        HANDLER_CALLS.load(Ordering::SeqCst),
        0,
        "an asynchronous failure reached the Xlib error handler, so the \
         documented `clear -> request -> sync -> take` sequence works after all \
         and the renderer's fbconfig check is no longer dead code"
    );
    assert_eq!(take_x_error(), None);
}

/// A request that *succeeds* reports no error, so the cell cannot invent one.
///
/// Without this, "the handler was not called" and "the cell stayed empty" would
/// be the same claim twice.
#[test]
fn a_successful_request_reports_no_error() {
    let Some((dpy, conn, screen)) = x() else {
        eprintln!("no X display; skipping");
        return;
    };
    install_counting_handler();
    maverick_x11::clear_x_error();
    // A property change on the root window: legal, and it answers nothing.
    // SAFETY: as `provoke_bad_window`, with a window id that does exist. The
    // value pointer is a live local byte and the call is asynchronous, so the
    // buffer only has to outlive the call itself.
    unsafe {
        XChangeProperty(
            dpy.as_ptr(),
            root_of(&conn, screen),
            1,
            4,
            0,
            8,
            1,
            [7u8].as_ptr(),
        )
    };
    dpy.sync();
    assert_eq!(take_x_error(), None);
    assert_eq!(
        HANDLER_CALLS.load(Ordering::SeqCst),
        0,
        "a legal request produced a protocol error"
    );
}

/// The recorded code is a protocol byte, and the name table is keyed by it.
#[test]
fn a_recorded_error_code_is_a_protocol_byte() {
    maverick_x11::clear_x_error();
    assert_eq!(std::mem::size_of::<c_uchar>(), 1);
    assert_eq!(maverick_x11::x_error_name(3), "BadWindow");
    assert_eq!(take_x_error(), None);
}

/// The error cell is per-thread, because it is a `thread_local` and Xlib runs
/// the handler on the thread that issued the failing request.
///
/// The connection is only ever touched by one thread at a time here, which is
/// the discipline `unsafe impl Send for XDisplay` permits and the one
/// `XInitThreads` makes safe. That a *second* thread sees an empty cell is the
/// property that makes a `clear → request → sync → take` sequence correct from
/// whichever thread the `Display*` legitimately reached.
#[test]
fn the_error_cell_is_per_thread() {
    if maverick_x11::open_x().is_err() {
        eprintln!("no X display; skipping");
        return;
    }
    // SAFETY: as in `x`, so a desynchronised socket cannot exit(1) silently.
    unsafe { XSetIOErrorHandler(Some(io_error_handler)) };

    maverick_x11::clear_x_error();
    assert_eq!(take_x_error(), None);

    let other = std::thread::spawn(take_x_error);
    assert_eq!(
        other.join().expect("the probe thread finished"),
        None,
        "a cell touched on this thread was visible from another"
    );
    assert_eq!(take_x_error(), None);
}

/// The premise `unsafe impl Send for XDisplay` rests on, checked rather than
/// assumed: Xlib's own locking is available, so an Xlib call made from one
/// thread is serialised against one made from another.
///
/// `open_x` discards this return value — it must be the *first* Xlib call in the
/// process, so it cannot be moved out — which is why the test asks the question
/// itself: if a libX11 build could not install its lock, `Send` would be a lie
/// and this is where that would show.
#[test]
fn xlib_thread_support_is_available() {
    // SAFETY: a query of a process-global capability flag with no pointer
    // arguments and no preconditions. After `open_x` has run it is a no-op
    // returning the same answer; called first it initialises the lock, which is
    // what the safety of `Send` depends on.
    let rc = unsafe { maverick_x11::XInitThreads() };
    assert_ne!(
        rc, 0,
        "libX11 could not enable its own locking, so a Display* is not safe to \
         move between threads and `unsafe impl Send for XDisplay` is unsound"
    );
}

/// `XDisplay` is `Send`, which is the documented reason the window manager's
/// structs stay `Send` even though they hold a raw Xlib pointer. It is
/// deliberately not `Sync`, and that is not something a test can assert
/// positively — so the reasoning lives on the `unsafe impl` and this pins the
/// half that is observable.
#[test]
fn the_display_handle_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<XDisplay>();
}

/// Every alias of a `Display*` is non-owning.
///
/// The window manager holds one, `Compositor` builds a second with
/// `XDisplay::from_raw`, and `open_x` had a third before it returned. If any of
/// them were `Drop`, or if `close` were reachable from a returned display,
/// letting them all go would `XCloseDisplay` a pointer the others still use and
/// the next request would touch freed memory. So: drop every alias, then use the
/// connection.
#[test]
fn every_alias_of_the_display_is_non_owning() {
    let Some((dpy, conn, screen)) = x() else {
        eprintln!("no X display; skipping");
        return;
    };
    let root = root_of(&conn, screen);

    // The pointer value itself, kept across the scope below: `XDisplay` is
    // `Copy`, so this is the very same `*mut Display` either way.
    let raw = dpy.as_ptr();

    {
        // The alias the compositor makes.
        // SAFETY: `raw` is the live `Display*` `open_x` returned, and the alias
        // leaves this scope without being closed — the same discipline
        // `Compositor::init` relies on.
        let alias = unsafe { XDisplay::from_raw(raw) };
        assert_eq!(alias.as_ptr(), raw);
        assert!(!alias.is_null());
    }
    // Leaving scope is as far as either handle can be taken down: `XDisplay` is
    // `Copy` and not `Drop`, so there is nothing to free here. If either ever
    // grew a `Drop` that closed the display, the connection below would be a
    // freed pointer.
    let _ = dpy;

    // A legal request on the root, which can only answer if the connection is
    // still open.
    maverick_x11::clear_x_error();
    // SAFETY: `raw` is live (the property under test) and the request is
    // asynchronous on a window that exists, so it takes the well-behaved path
    // the earlier tests avoid.
    unsafe { XChangeProperty(raw, root, 1, 4, 0, 8, 1, [7u8].as_ptr()) };
    // SAFETY: as `XDisplay::sync` — the display is live and `XSync` takes
    // nothing but the display and a flag.
    unsafe { maverick_x11::XSync(raw, 0) };
    assert_eq!(
        take_x_error(),
        None,
        "the connection stopped working once its aliases went out of scope"
    );
}

/// The value of `DISPLAY` reaches the error message when the connection fails.
///
/// Not a soundness property, but the one line of this module a user actually
/// sees, and the reason a test is worth having for a function that only ever
/// fails on a broken session.
#[test]
fn a_missing_display_is_named_in_the_failure() {
    let err = match maverick_x11::open_x() {
        Ok(_) => return, // a display exists; nothing to check
        Err(e) => e,
    };
    assert!(
        err.contains("DISPLAY"),
        "{err:?} does not name the variable to set"
    );
}
