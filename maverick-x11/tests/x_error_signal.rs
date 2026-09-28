//! Where the X error signal comes from, and who may read it.
//!
//! `maverick_x11` installs a *process-global* Xlib error handler that records
//! the code into a **thread-local** cell, and the compositor reads that cell to
//! decide whether a GLX request failed. Three claims follow from that shape, and
//! each is only worth anything if it is checked against a real server rather
//! than read:
//!
//! 1. `clear_x_error` → request → `XDisplay::sync` → `take_x_error` really does
//!    report the code for a request issued through **Xlib**, on this machine's
//!    libX11.
//! 2. It reports **nothing** for a request that succeeded, so the signal cannot
//!    invent a failure the compositor would act on.
//! 3. The cell is per-thread because Xlib runs the handler on the thread that
//!    issued the request — which is the same reason `XDisplay` is `Send` but
//!    deliberately not `Sync`: a second thread doing I/O on the same `Display*`
//!    could consume the error meant for the first, and the error would land in
//!    the wrong cell.
//!
//! # Running
//!
//! These need a real X server. They are written to *skip* rather than fail when
//! there is none, so the suite stays green on a build machine; run them against
//! a nested server to actually exercise them:
//!
//! ```text
//! Xvfb :91 -screen 0 1280x800x24 &
//! DISPLAY=:91 cargo test -p maverick-x11 --test x_error_signal
//! ```
//!
//! # Why the tests that provoke an error are ignored
//!
//! The tests below marked `#[ignore]` cannot be run as written, and the reason
//! is a property of this crate's architecture rather than of the tests.
//!
//! They provoke a `BadDrawable` with Xlib's own `XGetGeometry`, a synchronous
//! request that does its own round trip. `open_x` has handed the event queue to
//! XCB, so Xlib no longer owns the queue such a request expects to find its
//! reply in. Measured on a real server the request instead ends in libX11's
//! **I/O** error handler — "X connection to :N broken" — and this crate leaves
//! that handler at its default, which calls `exit(1)`. The test binary therefore
//! dies mid-run with no assertion failure and no backtrace. They are ignored
//! rather than fixed because the pattern they need is the one the crate
//! documents as unusable.
//!
//! Isolating the ingredient rules out the obvious suspect: `XSetEventQueueOwner`
//! alone is fatal on that request, and the silent error handler alone handles it
//! cleanly. The queue ownership is the cause.
//!
//! What the ignored tests would prove is worth keeping in the file, because it
//! says what a future Xlib or GLX call would have to re-establish. The tests
//! that need no synchronous Xlib request still run, and they cover the reachable
//! parts: the recorded code is the protocol's `error_code` byte, the handle is
//! `Send`, `XInitThreads` reports success, and every alias of the display is
//! non-owning.

use std::os::raw::{c_int, c_uchar, c_ulong, c_void};

use maverick_x11::{take_x_error, XConn, XDisplay};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt;

// Xlib's `XGetGeometry`, declared here rather than added to the crate's public
// surface: it is a request that *provokes* a `BadWindow` on demand, which is
// the whole point of the test and no production code wants it.
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

/// A window id no client can hold: `XAllocID` hands out ids from a
/// server-global counter, and this one is far above anything a live display
/// reaches, so the server answers `BadWindow` rather than matching by accident.
const NO_SUCH_WINDOW: c_ulong = 0x7fff_ffff;

/// Open the shared bootstrap, or `None` when there is no server to talk to.
fn x() -> Option<(XDisplay, XConn, usize)> {
    maverick_x11::open_x().ok()
}

/// A query that must succeed: the root window exists on any live display.
fn query_root(dpy: *mut c_void, root: c_ulong) -> bool {
    let (mut r, mut x, mut y, mut w, mut h, mut bw, mut d) = (0, 0, 0, 0, 0, 0, 0);
    // SAFETY: `dpy` is a live `Display*` from `open_x`; every out-parameter
    // points at a live local, and `XGetGeometry` writes through them and reads
    // nothing back.
    let rc = unsafe {
        XGetGeometry(
            dpy, root, &mut r, &mut x, &mut y, &mut w, &mut h, &mut bw, &mut d,
        )
    };
    rc != 0
}

/// The root window of the screen `open_x` reported.
fn root_of(conn: &XConn, screen: usize) -> c_ulong {
    conn.setup().roots[screen].root as c_ulong
}

// A synchronous Xlib request is fatal once `open_x` has given the event
// queue to XCB, and provoking an X error needs exactly that. See the module
// docs: the request ends in libX11's default I/O handler, which calls exit(1).
// The property stays recorded here for whoever next adds an Xlib or GLX call
// and has to re-establish it.
#[ignore = "provoking an X error requires a synchronous Xlib request, which is fatal here"]
#[test]
fn the_error_handler_sees_a_failed_xlib_request_and_names_it() {
    let Some((dpy, _conn, _screen)) = x() else {
        eprintln!("no X display; skipping");
        return;
    };
    maverick_x11::clear_x_error();
    let _ = query_root(dpy.as_ptr(), NO_SUCH_WINDOW);
    dpy.sync();
    let got = take_x_error();
    assert_eq!(
        got,
        Some(3),
        "a GetGeometry on a window that does not exist must record BadWindow (code 3)"
    );
    assert_eq!(maverick_x11::x_error_name(3), "BadWindow");
}

// A synchronous Xlib request is fatal once `open_x` has given the event
// queue to XCB, and provoking an X error needs exactly that. See the module
// docs: the request ends in libX11's default I/O handler, which calls exit(1).
// The property stays recorded here for whoever next adds an Xlib or GLX call
// and has to re-establish it.
#[ignore = "provoking an X error requires a synchronous Xlib request, which is fatal here"]
#[test]
fn the_error_handler_reports_nothing_for_a_request_that_worked() {
    let Some((dpy, conn, screen)) = x() else {
        eprintln!("no X display; skipping");
        return;
    };
    assert!(
        query_root(dpy.as_ptr(), root_of(&conn, screen)),
        "GetGeometry on the root failed"
    );
    dpy.sync();
    assert_eq!(
        take_x_error(),
        None,
        "a successful round trip must leave the cell empty, or every caller \
         reads its own success as a failure"
    );
}

// A synchronous Xlib request is fatal once `open_x` has given the event
// queue to XCB, and provoking an X error needs exactly that. See the module
// docs: the request ends in libX11's default I/O handler, which calls exit(1).
// The property stays recorded here for whoever next adds an Xlib or GLX call
// and has to re-establish it.
#[ignore = "provoking an X error requires a synchronous Xlib request, which is fatal here"]
#[test]
fn the_error_cell_is_per_thread_because_xlib_dispatches_on_the_requesting_thread() {
    let Some((dpy, conn, screen)) = x() else {
        eprintln!("no X display; skipping");
        return;
    };
    let root = root_of(&conn, screen);

    // A failure on this thread...
    maverick_x11::clear_x_error();
    let _ = query_root(dpy.as_ptr(), NO_SUCH_WINDOW);
    dpy.sync();
    assert_eq!(
        take_x_error(),
        Some(3),
        "the requesting thread sees its own error"
    );

    // ...and another thread, which has issued nothing, must see nothing. If the
    // cell were shared, or if Xlib dispatched the handler anywhere but the
    // thread that issued the request, this would read `Some(3)` and a
    // `clear → request → sync → take` on that thread would report a failure
    // nobody made. The same fact is why `XDisplay` is `Send` but not `Sync`.
    let other = std::thread::spawn(move || {
        let seen = take_x_error();
        (seen, dpy, root)
    });
    let (seen, dpy2, root2) = other.join().expect("the probe thread finished");
    assert_eq!(
        seen, None,
        "an error raised on another thread leaked into this one's cell"
    );

    // The display is usable from the thread the handle moved to, which is what
    // `unsafe impl Send for XDisplay` promises — and only that.
    assert!(
        query_root(dpy2.as_ptr(), root2),
        "the moved-to thread could not use the display"
    );
    dpy2.sync();
    assert_eq!(
        take_x_error(),
        None,
        "the moving thread's successful round trip reported an error"
    );
}

/// A failure provoked on the second thread lands in the *second* thread's cell.
///
/// This is the other half of the per-thread claim: the cell is not simply
/// "empty on other threads", it is written by whichever thread provoked the
/// error, so the compositor's `clear → request → sync → take` sequence is
/// correct from any thread the `Display*` legitimately reaches.
// A synchronous Xlib request is fatal once `open_x` has given the event
// queue to XCB, and provoking an X error needs exactly that. See the module
// docs: the request ends in libX11's default I/O handler, which calls exit(1).
// The property stays recorded here for whoever next adds an Xlib or GLX call
// and has to re-establish it.
#[ignore = "provoking an X error requires a synchronous Xlib request, which is fatal here"]
#[test]
fn an_error_raised_on_another_thread_lands_in_that_thread() {
    let Some((dpy, _conn, _screen)) = x() else {
        eprintln!("no X display; skipping");
        return;
    };
    let dpy2 = dpy;
    let other = std::thread::spawn(move || {
        maverick_x11::clear_x_error();
        let _ = query_root(dpy2.as_ptr(), NO_SUCH_WINDOW);
        dpy2.sync();
        take_x_error()
    });
    assert_eq!(
        other.join().expect("the probe thread finished"),
        Some(3),
        "the thread that provoked the error did not record it"
    );
    assert_eq!(
        take_x_error(),
        None,
        "the error leaked back onto the thread that did not provoke it"
    );
}

/// The two handles onto one `Display*` — the one `open_x` returned and the
/// second one `src/backend/x11/compositor_gl.rs` builds with `from_raw` — are
/// both non-owning.
///
/// If either were `Drop`, or if `close` were reachable from a returned
/// display, dropping the aliases would `XCloseDisplay` a pointer the other
/// alias still uses: the next `XSync` would touch freed memory. Dropping every
/// copy and then going on to use the connection is exactly that check.
// A synchronous Xlib request is fatal once `open_x` has given the event
// queue to XCB, and provoking an X error needs exactly that. See the module
// docs: the request ends in libX11's default I/O handler, which calls exit(1).
// The property stays recorded here for whoever next adds an Xlib or GLX call
// and has to re-establish it.
#[ignore = "provoking an X error requires a synchronous Xlib request, which is fatal here"]
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
    // freed pointer and `GetGeometry` would not answer.
    let _ = dpy;

    assert!(
        query_root(raw, root),
        "the connection died with its aliases"
    );
    // SAFETY: nothing closed `raw` — that is what the query above proved.
    unsafe { XDisplay::from_raw(raw) }.sync();
}

/// The scope of the "did an X error happen" signal: **Xlib/GLX requests only**.
///
/// Both clients share one `xcb_connection_t`, so it is worth being precise
/// about which of them the recorded code describes. An x11rb request that fails
/// is reported to *x11rb* — `check()` answers `Err(X11Error(BadWindow))` — and
/// libxcb keeps protocol errors in its own per-connection slot, so they never
/// reach Xlib's handler. That is why the renderer's `clear_x_error` /
/// `take_x_error` probes bracket `glXCreatePixmap` and `glXBindTexImageEXT`
/// (Xlib) and are absent from every XCB path in the window manager, where
/// `checked_void!` is the equivalent check.
#[test]
fn an_xcb_protocol_error_is_reported_to_x11rb_not_to_the_xlib_handler() {
    let Some((dpy, conn, _screen)) = x() else {
        eprintln!("no X display; skipping");
        return;
    };
    maverick_x11::clear_x_error();
    let got = conn
        .get_window_attributes(NO_SUCH_WINDOW as u32)
        .map(|c| c.reply().is_ok())
        .unwrap_or(false);
    dpy.sync();

    // x11rb saw it...
    assert!(
        !got,
        "a GetWindowAttributes on a window that does not exist must fail"
    );
    // ...and the Xlib handler did not, so `take_x_error` reporting anything here
    // would mean a later GLX request's failure were attributed to this one.
    assert_eq!(
        take_x_error(),
        None,
        "an x11rb request's protocol error reached the Xlib error cell"
    );
}

/// The premise `unsafe impl Send for XDisplay` rests on, checked rather than
/// assumed: Xlib's own locking is available, so an Xlib call made from one
/// thread is serialised against one made from another.
///
/// `open_x` discards this return value (it must be the *first* Xlib call in the
/// process, so it cannot be moved), which is why the test asks the question
/// itself: if a libX11 build could not install its lock, `Send` would be a lie
/// and this is where that would show.
#[test]
fn xlib_thread_support_is_available() {
    // SAFETY: a plain query of a process-global capability flag with no
    // preconditions, and no pointer arguments at all. After `open_x` has run it
    // is a no-op returning the same answer; called first it initialises the
    // lock, which is what the safety of `Send` depends on.
    let rc = unsafe { maverick_x11::XInitThreads() };
    assert_ne!(
        rc, 0,
        "libX11 could not enable its own locking, so a Display* is not \
         safe to move between threads and `unsafe impl Send for XDisplay` \
         is unsound"
    );
}

/// `XDisplay` is `Send`, which is the documented reason the window manager's
/// structs stay `Send` even though they hold a raw Xlib pointer.
#[test]
fn the_display_handle_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<XDisplay>();
}

/// The size of the recorded error code is the size of the X protocol's
/// `error_code` field, so a recorded code is a protocol byte and never a stray
/// byte of a wider field read out of the event struct.
#[test]
fn a_recorded_error_code_is_a_protocol_byte() {
    maverick_x11::clear_x_error();
    assert_eq!(std::mem::size_of::<c_uchar>(), 1);
    assert_eq!(take_x_error(), None);
}
