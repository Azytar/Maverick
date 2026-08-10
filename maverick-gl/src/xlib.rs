// maverick-gl/src/xlib.rs
// Hand-written FFI for the small slice of Xlib / libX11-xcb that Maverick
// needs. Same philosophy as `maverick-sys`: no binding-generator crates, only
// the exact symbols the code calls, each with the prototype copied from the
// system headers.
//
// Why Xlib at all in an otherwise pure-XCB window manager: GLX *is* an Xlib
// API. `glXMakeCurrent`, `glXSwapBuffers` and `glXBindTexImageEXT` all take a
// `Display*`, and there is no XCB equivalent that libGL will accept. The
// solution libX11 ships for exactly this case is `XGetXCBConnection`: open the
// display with Xlib, hand the *event queue* to XCB with `XSetEventQueueOwner`,
// and then drive every X request from x11rb over the same socket. That way
// there is one connection, one sequence-number space and one event queue.
//
// Golden rule enforced by this module's public API: after `open_x()` nobody
// may call an Xlib *event* function (`XNextEvent`, `XPending`, ...). Only GLX
// entry points and x11rb.

use std::cell::Cell;
use std::os::raw::{c_char, c_int, c_uchar, c_ulong, c_void};

/// Opaque `Display`. Xlib's struct layout is private in practice; we only ever
/// pass the pointer straight back to Xlib/GLX.
pub type Display = c_void;
/// `XID` — X resource ids (windows, pixmaps, ...). `unsigned long` in C.
pub type XID = c_ulong;

/// `XErrorEvent` from `X11/Xlib.h`. Only laid out so a custom error handler can
/// read the fields for logging; never constructed by us.
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

/// `enum XEventQueueOwner { XlibOwnsEventQueue = 0, XCBOwnsEventQueue }`
/// (`/usr/include/X11/Xlib-xcb.h`). There is no Rust enum for it, so the
/// literal is spelled out once, here.
pub const XCB_OWNS_EVENT_QUEUE: c_int = 1;

#[link(name = "X11")]
extern "C" {
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
    /// Error code of the most recent X error swallowed by
    /// [`silent_error_handler`], or `0` for "none since the last clear".
    ///
    /// Swallowing errors keeps the compositor alive, but it also means a
    /// genuinely wrong request (an fbconfig that does not match the pixmap's
    /// depth, say) produces a *silently broken* texture instead of a crash.
    /// This cell is what lets the few call sites that can actually be wrong —
    /// `glXCreatePixmap` above all — round-trip once and report the error.
    static LAST_X_ERROR: Cell<u8> = const { Cell::new(0) };
}

/// Swallow every asynchronous X error Xlib would otherwise route to its default
/// handler — which *prints and calls `exit(1)`*. A compositor races the client
/// constantly (a window can be destroyed between the `QueryTree` that listed it
/// and the `NameWindowPixmap` that redirects it), so `BadWindow`/`BadMatch`/
/// `BadDrawable` are normal traffic, not bugs. x11rb sees the same errors on
/// the shared queue and our dispatcher ignores them there too.
///
/// The code is recorded in [`LAST_X_ERROR`] so a caller that *can* tell a real
/// mistake from a race is able to look.
unsafe extern "C" fn silent_error_handler(_dpy: *mut Display, err: *mut XErrorEvent) -> c_int {
    if !err.is_null() {
        let code = (*err).error_code;
        LAST_X_ERROR.with(|c| c.set(code));
    }
    0
}

/// Forget any previously recorded X error. Call immediately before the request
/// you want to check.
pub fn clear_x_error() {
    LAST_X_ERROR.with(|c| c.set(0));
}

/// Take (and clear) the X error recorded since the last [`clear_x_error`].
///
/// Only meaningful after a round trip — [`XDisplay::sync`] — because X errors
/// are asynchronous.
pub fn take_x_error() -> Option<u8> {
    LAST_X_ERROR.with(|c| {
        let v = c.get();
        c.set(0);
        (v != 0).then_some(v)
    })
}

/// Name of a core X error code, for log messages (`XGetErrorText` needs the
/// display and allocates; these 17 codes are fixed by the protocol).
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
/// socket at exit — which is also what makes the compositor crash-safe: losing
/// the connection makes the X server undo the redirect and free the overlay all
/// by itself. Use [`XDisplay::close`] only when you can prove the connection is
/// already gone.
#[derive(Debug, Clone, Copy)]
pub struct XDisplay(*mut Display);

// The pointer is only ever touched from the WM thread; `Send` is needed purely
// so structs holding it stay `Send`.
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
    /// dropped, and no GLX resource may still be alive.
    pub unsafe fn close(self) {
        if !self.0.is_null() {
            XCloseDisplay(self.0);
        }
    }
}

/// Install the silent X error handler. Idempotent; called by [`open_x`].
pub fn install_silent_error_handler() {
    unsafe { XSetErrorHandler(Some(silent_error_handler)) };
}
