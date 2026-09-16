//! Shared X11 bootstrap — single XCB+Xlib connection for the whole process.
//!
//! Opens an Xlib `Display*`, hands its event queue to XCB with
//! `XSetEventQueueOwner(XCB_OWNS_EVENT_QUEUE)`, and wraps the underlying
//! `xcb_connection_t*` (via `XGetXCBConnection`) in `x11rb::xcb_ffi::XCBConnection`.
//! The WM core and the GLX/Vulkan backends share the same socket so there is
//! exactly one connection, one sequence-number space, and one event queue.
//!
//! # Ownership
//!
//! `open_x` returns `(XDisplay, XConn, screen)`. The `XConn` is constructed
//! with `should_drop = false` — it borrows the `Display*`'s
//! `xcb_connection_t*` and never calls `xcb_disconnect`. The `Display*` remains
//! the owner; the `XCBConnection` must not outlive it. In the window manager
//! the connection is shared as `Rc<XConn>` between the WM core and the
//! compositor so both see the same sequence space and event queue. `XDisplay`
//! is deliberately not `Drop` — closing it first would leave the borrowed
//! `Rc<XConn>` dangling. Both live for the process lifetime; the kernel closes
//! the socket at exit. `XDisplay::close` is only sound when every
//! `XCBConnection` wrapping this display is already dropped and no GLX/Vulkan
//! resource remains alive.
//!
//! # The golden rule
//!
//! After [`open_x`], **never** call an Xlib event function (`XNextEvent`,
//! `XPending`, `XPeekEvent`, ...). XCB owns the queue; Xlib would either block
//! forever or steal events the window manager needs. Only GLX/Vulkan entry
//! points and x11rb are allowed. `XSync` is fine (it flushes, it does not
//! dequeue).
//!
//! # Safety
//!
//! This crate contains `unsafe` blocks for every X11 FFI call. The safety
//! invariants are:
//! - `XDisplay` is not `Drop` because the `XCBConnection` borrows its
//!   `xcb_connection_t*` with `should_drop = false`; closing the display first
//!   would leave that connection dangling (see Ownership above).
//! - `silent_error_handler` records the error code synchronously; callers must
//!   follow `clear_x_error` → request → `XSync` → `take_x_error`.
//! - `XDisplay::close` must not be called while any `XCBConnection` wrapping
//!   this display is still alive.
//! - `from_raw` requires the pointer to be a valid `Display*` returned by
//!   `XOpenDisplay`.
//!
//! # Thread safety
//!
//! `XDisplay` is `Send` because the pointer is only ever touched from the WM
//! thread; the `Send` bound is needed purely so structs holding it stay
//! `Send`.

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

// `Send` is sound because `open_x` calls `XInitThreads()` before any other
// Xlib call; in practice the pointer is additionally only ever touched from
// the WM thread. Needed purely so structs holding it stay `Send`.
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

    /// Round-trip to the server, discarding queued events.
    ///
    /// Safe to call while XCB owns the queue: `XSync` only flushes and waits,
    /// it does not dequeue into Xlib's own buffer when `discard` is false.
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

/// Open the X display and return the pieces the window manager needs.
///
/// Returns `(display, connection, screen_number)`. The `XCBConnection` borrows
/// the display's connection (`should_drop = false`): the `Display*` stays the
/// owner, so nothing here ever calls `xcb_disconnect`. Both live for the whole
/// process.
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
            return Err("XGetXCBConnection returned NULL (libX11 built without XCB?)".into());
        }

        let conn = XCBConnection::from_raw_xcb_connection(raw, false)
            .map_err(|e| format!("x11rb could not wrap the xcb connection: {e}"))?;

        Ok((XDisplay::from_raw(dpy), conn, screen))
    }
}
