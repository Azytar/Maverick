//! The Xlib error handler is a process-global slot, and this crate **replaces**
//! whatever was in it rather than chaining to it.
//!
//! That is a decision, not an oversight, and it is the one thing about the error
//! path that cannot be verified by reading the `open_x` body: `XSetErrorHandler`
//! *returns* the previous handler and `open_x` throws that value away.
//!
//! The slot is observable without a request failing, because `XSetErrorHandler`
//! returns what it replaced — libX11 exports no `XGetErrorHandler`, so reading the
//! slot out by installing a probe over it is the only way. These tests therefore
//! assert that installing the silent handler puts the **same address** in the slot
//! every time, whatever was there before. A wrapper that chained to the previous
//! handler would be a third address and could not satisfy that; a replacement
//! always does.
//!
//! None of this needs a request to fail, which is deliberate: on this crate's
//! connection a *synchronous* Xlib request that fails desynchronises libXlib from
//! the shared socket and takes the process down (`io_error_scope.rs`, ignored by
//! default, measures that), so provoking errors is not something a test should
//! rely on. Whether a protocol error ever reaches the installed handler at all is
//! `x_error_signal.rs`'s question, and for asynchronous requests the answer is
//! "no".
//!
//! # Running
//!
//! ```text
//! Xvfb :91 -screen 0 1280x800x24 &
//! DISPLAY=:91 cargo test -p maverick-x11 --test error_handler_scope
//! ```
//! Without a display the one test that needs a server skips; the slot assertions
//! run either way.

use std::os::raw::{c_int, c_uchar, c_void};
use std::sync::{Mutex, MutexGuard, OnceLock};

use maverick_x11::{take_x_error, XDisplay};

/// Read the slot, the only way available: install `probe` and keep what
/// `XSetErrorHandler` says was there.
///
/// # Safety
/// `probe` must be a valid, process-lifetime `XErrorHandler`. The returned value
/// is the slot's previous occupant and is no longer installed once this returns,
/// so the caller must re-install whatever it wants afterwards.
unsafe fn slot_contents(
    probe: unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int,
) -> Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int> {
    // SAFETY: the caller guarantees `probe` satisfies the contract above, and
    // `XSetErrorHandler` takes no other argument and reads nothing.
    unsafe { XSetErrorHandler(Some(probe)) }
}

/// A stand-in for "some other library's handler", addressable so the test can
/// recognise the slot afterwards.
///
/// # Safety
/// Never called: every read of the slot is immediately overwritten, and a
/// surviving installation is removed by `SilentHandlerOnDrop`.
unsafe extern "C" fn probe(_dpy: *mut c_void, _err: *mut c_void) -> c_int {
    0
}

/// `slot_contents` on `probe`, named for readability at the call sites.
fn slot() -> Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int> {
    // SAFETY: `probe` is a non-capturing `extern "C"` fn item with the signature
    // `XSetErrorHandler` declares and the code it points at is mapped for the
    // process lifetime, which is the only thing the registration needs.
    unsafe { slot_contents(probe) }
}

/// The slot is one per process and `cargo test` runs a binary's tests in parallel
/// threads, so without this every test would be racing the others to overwrite
/// the same slot and reading back whatever the last thread installed. Tests that
/// need no display take it too: "the same address as a moment ago" only means
/// something if nothing else moved it in between.
fn exclusive() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Puts the crate's own handler back however this test leaves the process.
struct SilentHandlerOnDrop;

impl Drop for SilentHandlerOnDrop {
    fn drop(&mut self) {
        maverick_x11::install_silent_error_handler();
    }
}

#[link(name = "X11")]
extern "C" {
    fn XSetErrorHandler(
        h: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int>,
    ) -> Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int>;
}

// Comparing two `unsafe extern "C" fn` pointers is what this file is for, and the
// lint that objects is about the opposite direction.
//
// `unpredictable_function_pointer_comparisons` warns that two *distinct source*
// functions may be merged by the linker into one address, so "these differ" can
// be wrong. What these tests ask is the reverse: whether the slot holds the same
// address it held a moment ago. A merge cannot make a freshly built wrapper equal
// the function it wraps — different contents, so different code — so equality
// here means "the same code is installed" and inequality means "different code is
// installed", which is exactly the question about chaining.
#[test]
#[allow(unpredictable_function_pointer_comparisons)]
fn installing_the_silent_handler_replaces_the_slot_and_does_not_wrap_it() {
    let _guard = exclusive();
    let _restore = SilentHandlerOnDrop;

    // What a *fresh* install puts in the slot, read out with the probe.
    maverick_x11::install_silent_error_handler();
    let fresh = slot();
    assert!(
        fresh.is_some(),
        "the silent handler left the slot empty, so every comparison below would \
         be comparing two empties and would pass for the wrong reason"
    );
    assert_ne!(
        fresh,
        Some(probe as unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int),
        "a fresh install left the probe in the slot, so the comparisons below \
         prove nothing"
    );

    // Now install the probe and put the silent handler over it — the situation a
    // second library in the process would create.
    let _ = slot();
    maverick_x11::install_silent_error_handler();
    let after_replacing = slot();

    assert_eq!(
        after_replacing, fresh,
        "installing over a foreign handler produced a different address than a \
         fresh install does: a wrapper was installed that chains to whatever was \
         there, instead of the crate's own handler replacing it"
    );
}

#[test]
#[allow(unpredictable_function_pointer_comparisons)]
fn installing_the_silent_handler_twice_leaves_the_same_handler_in_the_slot() {
    let _guard = exclusive();
    let _restore = SilentHandlerOnDrop;

    maverick_x11::install_silent_error_handler();
    let first = slot();
    maverick_x11::install_silent_error_handler();
    let second = slot();

    assert!(first.is_some(), "the first install left the slot empty");
    assert_eq!(
        first, second,
        "a second install displaced the handler the first one installed, so the \
         function called on an X error is no longer the one the crate documents"
    );
}

#[test]
#[allow(unpredictable_function_pointer_comparisons)]
fn open_x_replaces_a_handler_that_was_already_installed() {
    let _guard = exclusive();
    if maverick_x11::open_x().is_err() {
        eprintln!("no X display; skipping");
        return;
    }
    let _restore = SilentHandlerOnDrop;

    // The address a fresh silent install produces, for comparison below.
    maverick_x11::install_silent_error_handler();
    let fresh = slot();
    assert!(fresh.is_some(), "the silent handler left the slot empty");

    // Leave the probe installed, then let `open_x` run over it. The WM calls
    // `open_x` once, but the contract should hold however many times it runs.
    let _ = slot();
    let (dpy, _conn, _screen): (XDisplay, maverick_x11::XConn, usize) =
        maverick_x11::open_x().expect("a second connection to the same server");
    let after_open = slot();

    assert_eq!(
        after_open, fresh,
        "open_x installed a different address than a fresh silent install does, so \
         it chained to the handler that was already there instead of replacing it"
    );

    // The connection still works, so the test did not succeed by breaking the
    // display on the way past.
    dpy.sync();
    assert_eq!(take_x_error(), None);
}

/// The recorded code is the protocol's `error_code` byte, not a widened field.
///
/// `XErrorEvent` is declared `#[repr(C)]` with the same field types and order as
/// the C struct, so reading `error_code` at the C offset reads the right byte.
/// Every code is in `1..=255`, so the *offset* is what pins this, and the offset
/// is what the ignored `io_error_scope.rs` checks by provoking a real
/// `BadWindow`.
#[test]
fn the_recorded_code_is_the_protocol_error_code_byte() {
    maverick_x11::clear_x_error();
    assert_eq!(std::mem::size_of::<c_uchar>(), 1);
    assert_eq!(maverick_x11::x_error_name(3), "BadWindow");
    assert_eq!(take_x_error(), None);
}
