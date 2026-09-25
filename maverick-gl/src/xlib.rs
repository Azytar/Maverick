// Hand-written FFI for the small slice of Xlib / libX11-xcb that the compositor
// needs: no binding-generator crates, only the exact symbols the code calls,
// each with the prototype copied from the system headers.
//
// Xlib is unavoidable in an otherwise pure-XCB window manager because GLX *is*
// an Xlib API — `glXMakeCurrent`, `glXSwapBuffers` and `glXBindTexImageEXT` all
// take a `Display*`, and libGL accepts no XCB equivalent. The one-connection
// arrangement that reconciles that with an XCB-based WM is documented at the
// crate root; what this module's public API enforces is that after `open_x()`
// only GLX entry points and x11rb may touch the connection — no Xlib *event*
// function (`XNextEvent`, `XPending`, ...) may ever run.

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
/// handler, which prints it and calls `exit(1)`. A compositor races the client
/// constantly — a window can be destroyed between the `QueryTree` that listed it
/// and the `NameWindowPixmap` that redirects it — so `BadWindow`/`BadMatch`/
/// `BadDrawable` are normal traffic, not bugs. x11rb sees the same errors on
/// the shared queue and the WM's dispatcher ignores them there too.
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
/// Deliberately **not** `Drop`: the `XCBConnection` handed out by [`crate::open_x`]
/// borrows this display's `xcb_connection_t*` with `should_drop = false`, so
/// closing the display first would leave that connection dangling. The window
/// manager holds both for the whole process lifetime and the kernel closes the
/// socket at exit — which is also what makes the compositor crash-safe: losing
/// the connection makes the X server undo the redirect and free the overlay all
/// by itself. Use [`XDisplay::close`] only when you can prove the connection is
/// already gone.
#[derive(Debug, Clone, Copy)]
pub struct XDisplay(*mut Display);

// `Send` is sound because `open_x` calls `XInitThreads()` before any other
// Xlib call, enabling Xlib's internal locking; in practice the pointer is
// additionally only ever touched from the WM thread. `Send` is needed purely
// so structs holding it stay `Send`. (`Sync` is NOT granted: concurrent
// `&`-shared Xlib calls would still need external locking.)
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
    /// Safe to call while XCB owns the queue, but only because `discard` is
    /// `0`: `XSync` then flushes and waits without touching the connection's
    /// event queue, so nothing the WM still needs is thrown away.
    /// `XSync(dpy, 1)` would silently drop those events.
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

/// Install the silent X error handler. Idempotent; called by [`crate::open_x`].
pub fn install_silent_error_handler() {
    unsafe { XSetErrorHandler(Some(silent_error_handler)) };
}

/// The one piece of bookkeeping in this module: what a caller is allowed to
/// conclude from [`take_x_error`], over any sequence of the operations the
/// sanctioned `clear → request → sync → take` round trip is made of.
///
/// Nothing here needs a connection. The handler only ever reads
/// `error_code`, so a fabricated event is exactly as good as a real one, and
/// the cell it writes is thread-local, so a test never sees another thread's
/// errors.
#[cfg(test)]
mod property_tests {
    use super::*;
    use proptest::prelude::*;

    /// Hand the error cell a code the way Xlib would, through the real
    /// handler, without a `Display*` to report it on.
    fn record(code: u8) {
        let mut ev = XErrorEvent {
            type_: 0,
            display: std::ptr::null_mut(),
            resourceid: 0,
            serial: 0,
            error_code: code,
            request_code: 0,
            minor_code: 0,
        };
        // SAFETY: the handler dereferences `err` and reads `display` and
        // `error_code` only; both are valid here, and `display` is never
        // touched.
        let rc = unsafe { silent_error_handler(std::ptr::null_mut(), &mut ev) };
        assert_eq!(rc, 0, "the handler must never ask Xlib to exit");
    }

    proptest! {
        /// Whatever sequence of round trips went through, a take reports
        /// exactly what the last error was, reports it once, and reports
        /// nothing for a round trip that was clean.
        ///
        /// The discipline around it is the contract: a caller clears, issues
        /// the request that could be wrong, syncs, and takes. A take that did
        /// not clear would report the previous frame's `BadMatch` against
        /// every later pixmap, and the compositor would drop windows it had
        /// just composited. Code 0 means "no error since the last clear", so
        /// reporting it would turn a clean round trip into a failure.
        #[test]
        fn a_taken_x_error_is_reported_exactly_once(steps in prop::collection::vec(any::<u8>(), 1..24)) {
            clear_x_error();
            prop_assert_eq!(take_x_error(), None, "a cleared cell holds nothing");
            for code in steps {
                record(code);
                if code == 0 {
                    prop_assert_eq!(take_x_error(), None, "0 is not an error code");
                } else {
                    prop_assert_eq!(take_x_error(), Some(code));
                }
                prop_assert_eq!(take_x_error(), None, "a taken error was reported again");
                // A round trip that is never checked must not leave anything
                // behind for the next one either.
                clear_x_error();
                prop_assert_eq!(take_x_error(), None);
            }
        }
    }

    /// A clear forgets whatever the previous round trip recorded, so a stale
    /// error cannot be blamed on the request that follows it.
    #[test]
    fn clearing_x_error_discards_the_recorded_code() {
        for code in [1u8, 8, 11, 17, 200, 255] {
            record(code);
            clear_x_error();
            assert_eq!(take_x_error(), None, "code {code} survived a clear");
        }
    }

    /// Each of the 17 core protocol codes has its own name, and nothing else
    /// does.
    ///
    /// The name is the whole of what a user sees when a request the compositor
    /// made was refused, so two codes sharing one name (or a known code falling
    /// through to the extension catch-all) makes the log point at the wrong
    /// problem — and the table is fixed by the protocol, never extended by a
    /// driver.
    #[test]
    fn every_core_x_error_code_has_its_own_name() {
        const CORE: u8 = 17;
        let mut seen: Vec<&str> = Vec::new();
        for code in 1..=CORE {
            let name = x_error_name(code);
            assert!(!name.is_empty(), "code {code} has no name");
            assert!(
                !seen.contains(&name),
                "code {code} is reported as {name:?}, which another code already uses"
            );
            assert_ne!(
                name,
                x_error_name(0),
                "a core code fell through to the extension catch-all"
            );
            seen.push(name);
        }
        // Everything the protocol does not define is an extension's, and says
        // so — a name that guesses would be worse than none.
        for code in (CORE + 1)..=u8::MAX {
            assert_eq!(
                x_error_name(code),
                x_error_name(0),
                "code {code} guessed a name"
            );
        }
    }
}
