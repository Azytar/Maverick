//! Shared X11 bootstrap — one Xlib display whose event queue is owned by XCB.
//!
//! `open_x` opens a `Display*`, hands the queue to XCB with
//! `XSetEventQueueOwner(XCB_OWNS_EVENT_QUEUE)`, and wraps the display's own
//! `xcb_connection_t*` (`XGetXCBConnection`) in `x11rb::xcb_ffi::XCBConnection`.
//! The WM core and the compositor share that single socket, so there is exactly
//! one sequence-number space and one event queue.
//!
//! # Ownership
//!
//! `XConn` is built with `should_drop = false`: it borrows the display's
//! connection and never calls `xcb_disconnect`. The `Display*` remains the
//! owner and the connection must not outlive it, so the window manager keeps
//! both for the process lifetime and hands the connection around as
//! `Rc<XConn>`; the kernel closes the socket at exit. See [`XDisplay`] for why
//! it is deliberately not `Drop`.
//!
//! # The golden rule
//!
//! After [`open_x`], **never** call an Xlib event function (`XNextEvent`,
//! `XPending`, `XPeekEvent`, ...). XCB owns the queue; Xlib would either block
//! forever or steal events the window manager needs. `XSync` is safe: it
//! flushes and waits, it never dequeues.
//!
//! # Error handling
//!
//! The silent error handler exists because a window manager races clients by
//! nature (a window can die between the query that listed it and the request
//! that redirects it), so X errors are routine — while Xlib's default handler
//! terminates the process. The handler records the code synchronously, and
//! because X errors are asynchronous the only correct read sequence is
//! `clear_x_error` → request → [`XDisplay::sync`] → `take_x_error`.
//!
//! # Thread safety
//!
//! [`XDisplay`] is `Send` so the window-manager structs holding it stay
//! `Send`; the pointer itself is only ever touched from the WM thread.

use std::cell::Cell;
use std::os::raw::{c_char, c_int, c_uchar, c_ulong, c_void};

use x11rb::xcb_ffi::XCBConnection;

pub type XConn = XCBConnection;

/// Opaque Xlib `Display*`.
pub type Display = c_void;
/// X resource ids.
pub type XID = c_ulong;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct XErrorEvent {
    pub type_: c_int,
    pub display: *mut Display,
    pub resourceid: XID,
    pub serial: c_ulong,
    pub error_code: c_uchar,
    pub request_code: c_uchar,
    pub minor_code: c_uchar,
}

pub type XErrorHandler = Option<unsafe extern "C" fn(*mut Display, *mut XErrorEvent) -> c_int>;

pub const XCB_OWNS_EVENT_QUEUE: c_int = 1;

#[link(name = "X11")]
extern "C" {
    pub fn XInitThreads() -> c_int;
    pub fn XOpenDisplay(name: *const c_char) -> *mut Display;
    pub fn XCloseDisplay(dpy: *mut Display) -> c_int;
    pub fn XDefaultScreen(dpy: *mut Display) -> c_int;
    pub fn XFree(data: *mut c_void) -> c_int;
    pub fn XSync(dpy: *mut Display, discard: c_int) -> c_int;
    pub fn XSetErrorHandler(handler: XErrorHandler) -> XErrorHandler;
}

#[link(name = "X11-xcb")]
extern "C" {
    pub fn XGetXCBConnection(dpy: *mut Display) -> *mut c_void;
    pub fn XSetEventQueueOwner(dpy: *mut Display, owner: c_int);
}

thread_local! {
    static LAST_X_ERROR: Cell<u8> = const { Cell::new(0) };
}

unsafe extern "C" fn silent_error_handler(_dpy: *mut Display, err: *mut XErrorEvent) -> c_int {
    if !err.is_null() {
        LAST_X_ERROR.with(|c| c.set((*err).error_code));
    }
    0
}

/// Install the silent X error handler. Idempotent.
pub fn install_silent_error_handler() {
    unsafe { XSetErrorHandler(Some(silent_error_handler)) };
}

/// Forget any previously recorded X error. Call immediately before the request
/// you want to check.
pub fn clear_x_error() {
    LAST_X_ERROR.with(|c| c.set(0));
}

/// Take (and clear) the X error recorded since the last `clear_x_error`.
///
/// Only meaningful after a round trip — `XDisplay::sync` — because X errors
/// are asynchronous.
pub fn take_x_error() -> Option<u8> {
    LAST_X_ERROR.with(|c| {
        let v = c.get();
        c.set(0);
        (v != 0).then_some(v)
    })
}

/// Name of a core X error code.
pub fn x_error_name(code: u8) -> &'static str {
    match code {
        1 => "BadRequest",
        2 => "BadValue",
        3 => "BadWindow",
        4 => "BadPixmap",
        5 => "BadAtom",
        6 => "BadCursor",
        7 => "BadFont",
        8 => "BadMatch",
        9 => "BadDrawable",
        10 => "BadAccess",
        11 => "BadAlloc",
        12 => "BadColor",
        13 => "BadGC",
        14 => "BadIDChoice",
        15 => "BadName",
        16 => "BadLength",
        17 => "BadImplementation",
        _ => "X error (extension)",
    }
}

/// Owned handle to the Xlib `Display*`.
///
/// Deliberately **not** `Drop`: the `XCBConnection` handed out by [`open_x`]
/// borrows this display's `xcb_connection_t*` with `should_drop = false`, so
/// closing the display first would leave that connection dangling. The window
/// manager holds both for the whole process lifetime and the kernel closes the
/// socket at exit. Use [`XDisplay::close`] only when you can prove the
/// connection is already gone.
#[derive(Debug, Clone, Copy)]
pub struct XDisplay(*mut Display);

// `Send` is sound because `open_x` runs `XInitThreads()` before any other
// Xlib call, so Xlib's own locking is active; in practice the pointer is
// additionally only ever touched from the WM thread. The bound exists purely so
// the structs holding it stay `Send`.
unsafe impl Send for XDisplay {}

impl XDisplay {
    /// Wrap a raw `Display*`.
    ///
    /// # Safety
    /// `ptr` must be a live `Display*` returned by `XOpenDisplay`.
    pub unsafe fn from_raw(ptr: *mut Display) -> Self {
        Self(ptr)
    }

    #[inline]
    pub fn as_ptr(self) -> *mut Display {
        self.0
    }

    #[inline]
    pub fn is_null(self) -> bool {
        self.0.is_null()
    }

    /// Round-trip to the server and wait for its reply.
    ///
    /// Safe to call while XCB owns the queue: `XSync` flushes and waits, it
    /// never dequeues into Xlib's own buffer, so the events stay in XCB where
    /// the window manager reads them.
    pub fn sync(self) {
        unsafe { XSync(self.0, 0) };
    }

    /// Explicitly close the display.
    ///
    /// # Safety
    /// Every `XCBConnection` wrapping this display's connection must already be
    /// dropped, and no GLX/Vulkan resource may still be alive.
    pub unsafe fn close(self) {
        if !self.0.is_null() {
            XCloseDisplay(self.0);
        }
    }
}

/// Open the X display and return `(display, connection, screen_number)`.
///
/// # Error semantics
///
/// Every `Err` return leaves no `Display*` behind: once `XOpenDisplay` has
/// succeeded, each remaining failure point closes the display before
/// returning, so a failed call owns no socket, no server connection and no
/// Xlib buffers. A caller that retries `open_x` never accumulates fds.
///
/// # Ownership
///
/// On success the `XCBConnection` borrows the display's connection
/// (`should_drop = false`), so the `Display*` stays the owner and the caller
/// must keep **both** alive for as long as either is used; see the crate docs
/// for the lifetime rules that follow from that.
pub fn open_x() -> Result<(XDisplay, XConn, usize), String> {
    unsafe {
        // First Xlib call in the process (see `Send` docs above).
        XInitThreads();
        let dpy = XOpenDisplay(std::ptr::null());
        if dpy.is_null() {
            let target = std::env::var("DISPLAY").unwrap_or_else(|_| "<unset>".into());
            return Err(format!("cannot open X display (DISPLAY={target})"));
        }

        install_silent_error_handler();
        XSetEventQueueOwner(dpy, XCB_OWNS_EVENT_QUEUE);

        let screen = XDefaultScreen(dpy) as usize;
        let raw = XGetXCBConnection(dpy);
        if raw.is_null() {
            // SAFETY: `XGetXCBConnection` never produced a connection, so no
            // `XCBConnection` can be borrowing this display and `close`'s
            // precondition holds vacuously. Keeping the display open here would
            // buy nothing: the process-lifetime leak policy exists to protect a
            // live borrower, and this path has none.
            XDisplay::from_raw(dpy).close();
            return Err("XGetXCBConnection returned NULL (libX11 built without XCB?)".into());
        }

        let conn = XCBConnection::from_raw_xcb_connection(raw, false).map_err(|e| {
            // SAFETY: the wrap failed, so there is no `XCBConnection` outliving
            // this call. x11rb received `should_drop = false`, so its failure
            // path drops the wrapper *without* `xcb_disconnect` — the
            // `xcb_connection_t*` is still the display's to release, which makes
            // this close the only remaining way to free the socket.
            XDisplay::from_raw(dpy).close();
            format!("x11rb could not wrap the xcb connection: {e}")
        })?;

        Ok((XDisplay::from_raw(dpy), conn, screen))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The core X error codes: `BadRequest` (1) through `BadImplementation`
    /// (17), as defined by the protocol. Anything outside that range is an
    /// extension's own code, which this crate has no name for.
    const FIRST_CORE: u8 = 1;
    const LAST_CORE: u8 = 17;

    proptest! {
        /// Totality: the recorded code is an opaque byte from the server, and
        /// this is the only place it becomes human-readable, so every one of the
        /// 256 values must get a name — including 0 (the "no error" sentinel
        /// `take_x_error` filters out) and the extension codes no core table
        /// covers. An unnamed code would have to be printed as a bare number in
        /// the log line that explains why a request was dropped.
        #[test]
        fn every_error_byte_has_a_name(code in any::<u8>()) {
            prop_assert!(
                !x_error_name(code).is_empty(),
                "X error code {code} has no name to log"
            );
        }

        /// Distinctness: a log that prints `BadWindow` for a `BadPixmap` sends
        /// whoever reads it after the wrong problem, and a core code that shares
        /// the extension bucket hides that the failure was a *known* one. So each
        /// core code must be distinguishable from all 255 other byte values.
        #[test]
        fn each_core_error_code_has_a_name_of_its_own(code in FIRST_CORE..=LAST_CORE) {
            let name = x_error_name(code);
            for other in 0u8..=u8::MAX {
                if other != code {
                    prop_assert_ne!(
                        x_error_name(other),
                        name,
                        "error codes {} and {} share the name {:?}",
                        code,
                        other,
                        name
                    );
                }
            }
        }

        /// The error observer must never invent a failure. `take_x_error` is the
        /// only signal the X sinks branch on — it is what turns a request into
        /// "the client died" or "retry" — so a fabricated code would make the WM
        /// discard a perfectly good request, and taking twice must not report the
        /// same failure twice.
        #[test]
        fn taking_an_x_error_never_invents_one(rounds in 0usize..8) {
            for _ in 0..rounds {
                clear_x_error();
                prop_assert_eq!(take_x_error(), None, "no error was recorded since the clear");
                prop_assert_eq!(take_x_error(), None, "take must clear what it reported");
            }
        }
    }
}
