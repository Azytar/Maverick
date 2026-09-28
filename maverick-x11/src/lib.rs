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
//! terminates the process. The handler records the code synchronously, so the
//! intended read sequence is `clear_x_error` → request → [`XDisplay::sync`] →
//! `take_x_error`.
//!
//! **That sequence does not actually work on this crate's connection, and the
//! docs on [`XDisplay::sync`] say so with the measurement.** `open_x` gives the
//! event queue to XCB, so libXlib never reads protocol errors off the socket —
//! they are left for x11rb — and the handler is not called for a request the
//! server rejected. Two things follow, and both are load-bearing for callers:
//!
//! * The recorded code describes **Xlib and GLX requests only** *and* only ones
//!   libXlib reads. Protocol errors on requests x11rb issues are reported by
//!   libxcb to x11rb itself (`Cookie::reply` answers
//!   `Err(ReplyError::X11Error(..))`), which is what the window manager's XCB
//!   paths use `checked_void!` for.
//! * The compositor's GLX probes with `take_x_error` are **best-effort**: they
//!   cannot see a failure, so a caller must not branch on a negative answer as
//!   though it meant the request succeeded. In `maverick-gl`'s renderer that
//!   makes the per-visual fbconfig check a no-op; see
//!   `Renderer::texture_from_pixmap`.
//!
//! `tests/x_error_signal.rs` measures all of this against a real server rather
//! than asserting it, so a change in either direction is visible.
//!
//! # A synchronous Xlib request cannot be used to provoke an X error
//!
//! Measured on a real server, the two statements above leave production with no
//! way to *populate* this cell, and one obvious way to try that is fatal.
//!
//! [`open_x`] hands the event queue to XCB
//! (`XSetEventQueueOwner(dpy, XCB_OWNS_EVENT_QUEUE)`) so the window manager and
//! the compositor share one sequence-number space. A synchronous Xlib request —
//! one that does its own round trip, such as `XGetGeometry` — expects to find its
//! reply in a queue Xlib no longer owns. Issuing one against a drawable that
//! produces `BadDrawable` was measured to end in
//!
//! ```text
//! X connection to :71 broken (explicit kill or server shutdown).
//! ```
//!
//! which is libX11's **I/O** error handler, not its error handler. This crate
//! replaces the error handler and leaves the I/O one at its default, and the
//! default I/O handler calls `exit(1)`: the process goes down with no unwinding,
//! no `Drop`, and no cleanup. Isolating the ingredient confirms the queue
//! ownership is the cause and the silent handler is not — with
//! `XSetEventQueueOwner` alone the same request is fatal, and with the silent
//! handler alone the same request returns an error the handler swallows.
//!
//! Two consequences a caller has to know:
//!
//! * **Do not reach for a synchronous Xlib request to produce an X error.** Use
//!   an x11rb request and read `ReplyError::X11Error` from it, which is what
//!   `checked_void!` does. `XSync` is the one Xlib round trip that is safe,
//!   because it cannot itself be the request that failed.
//! * **The cell is therefore effectively write-only from production's point of
//!   view.** Every request that can carry an error in the window manager goes
//!   through x11rb, so `take_x_error` returns `None` there by construction. It
//!   remains as a guard for a future Xlib or GLX call, and a caller that adds
//!   one must verify it does not round-trip.
//!
//! The tests that would have to provoke an error through Xlib are
//! `#[ignore]`d for this reason; see `tests/x_error_signal.rs`.
//!
//! # Thread safety
//!
//! [`XDisplay`] is `Send` and deliberately **not** `Sync`; the reasoning is on
//! the `unsafe impl` itself. `open_x` enables Xlib's own locking with
//! `XInitThreads()` before anything else and refuses to hand out a display if
//! that fails, which is what backs the `Send` bound. The pointer is only ever
//! touched from one thread: Xlib dispatches protocol errors on the thread that
//! issued the request, and a second thread doing I/O on the same `Display*`
//! could consume an error meant for the first — which is why sharing it would
//! need `Sync` and does not get it.

use std::cell::Cell;
use std::os::raw::{c_char, c_int, c_uchar, c_ulong, c_void};

use x11rb::xcb_ffi::XCBConnection;

pub type XConn = XCBConnection;

/// Opaque Xlib `Display*`.
pub type Display = c_void;
/// X resource ids.
pub type XID = c_ulong;

/// Xlib's `XErrorEvent`, the 32-byte error record the handler is handed.
///
/// `#[repr(C)]` with the same field types and order as the C struct, so the
/// field offsets the handler reads match what libX11 wrote:
///
/// ```c
/// typedef struct {
///     int type; Display *display; XID resourceid;
///     unsigned long serial; unsigned char error_code;
///     unsigned char request_code; unsigned char minor_code;
/// } XErrorEvent;
/// ```
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

// The record cell is per-thread because Xlib runs the handler on the thread
// that issued the failing request: it is called from inside that thread's
// request/flush/reply path, never from a background reader. A shared cell
// would let one thread's `clear → request → sync → take` sequence read another
// thread's failure — and it would be a data race besides, since the handler
// runs from C at a point Rust cannot see.
//
// The handler is a plain `fn` item, so `silent_error_handler` is an ordinary
// non-capturing function pointer of exactly the type `XErrorHandler` names and
// is valid for the life of the process: the code lives in `.text`, it is never
// unregistered, and the cell it writes has a `const` initialiser, so first
// access allocates nothing and registers no destructor. libX11 is linked, never
// `dlclose`d, so the registration cannot outlive the code it points at.
//
// Returning `0` is load-bearing, not a convention: Xlib calls `exit()` when a
// client-installed error handler returns non-zero. That is the whole reason
// this handler exists — the default one prints the error and takes the window
// manager down with it, which a compositor cannot afford when a client simply
// unmapped a window between two requests.
unsafe extern "C" fn silent_error_handler(_dpy: *mut Display, err: *mut XErrorEvent) -> c_int {
    if !err.is_null() {
        LAST_X_ERROR.with(|c| c.set((*err).error_code));
    }
    0
}

/// Install the silent X error handler. Idempotent.
///
/// # What discarding the previous handler means
///
/// The Xlib error handler is **process-global** — one slot for the whole
/// process, not one per `Display*` — and `XSetErrorHandler` returns whatever
/// was installed. That return value is deliberately dropped, and that is the
/// correct choice rather than a leak of responsibility:
///
/// * **Chaining to the previous handler would reintroduce the bug this
///   replaces.** libX11's default handler prints the protocol error and then
///   calls `exit()`. Any handler that chains to it kills the window manager on
///   the first `BadWindow`, which for a compositor is a normal event.
/// * **A window manager is the process.** Nothing else in it installs an error
///   handler: `open_x` is the only caller and the only one that ever opens a
///   `Display*`, so there is no foreign handler to preserve.
///
/// The "did an X error happen" signal therefore *is* [`take_x_error`], and the
/// caller contract is the sequence documented on this module. `open_x` is the
/// only place that installs it; [`install_silent_error_handler`] is public
/// because installing it a second time has to be harmless, not because a second
/// owner is expected.
pub fn install_silent_error_handler() {
    // SAFETY: `silent_error_handler` is a non-capturing `extern "C"` fn item
    // whose signature is the one `XSetErrorHandler` declares, so it is a valid
    // argument of type `XErrorHandler`; it stays mapped for the process
    // lifetime, which is as long as libX11 will ever call it.
    unsafe { XSetErrorHandler(Some(silent_error_handler)) };
}

/// Forget any previously recorded X error. Call immediately before the request
/// you want to check.
pub fn clear_x_error() {
    LAST_X_ERROR.with(|c| c.set(0));
}

/// Take (and clear) the X error recorded since the last [`clear_x_error`].
///
/// Only meaningful after a round trip — [`XDisplay::sync`] — because X errors are
/// asynchronous. On the connection [`open_x`] builds, see that function's docs:
/// libXlib does not read protocol errors there, so this in practice answers
/// `None` whatever the server said.
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
///
/// Being `Copy` and not `Drop` is what makes the ownership graph acyclic: the
/// window manager's `dpy` field, the compositor's handle built with
/// [`XDisplay::from_raw`] and the local in `open_x` are all non-owning aliases
/// of one `Display*`, so there is no path on which two of them can each decide
/// to close it. [`close`](XDisplay::close) is the only closer, and nothing the
/// crate returns can reach it.
#[derive(Debug, Clone, Copy)]
pub struct XDisplay(*mut Display);

// `Send` — and deliberately *not* `Sync`.
//
// What backs the claim: `open_x` calls `XInitThreads()` as the very first
// Xlib call in the process and now *fails* if it does not succeed, so by the
// time an `XDisplay` exists, Xlib's own per-display lock is installed and a
// call into Xlib from thread B cannot race one from thread A. Without that call
// this `impl` would be unsound, which is why [`XDisplay::from_raw`] carries the
// same requirement in its safety contract.
//
// Why `Sync` is withheld rather than granted: the lock makes Xlib's internal
// state safe, not Xlib's *semantics* on one display. Two threads issuing
// requests through `&Display` would interleave arbitrarily, and — the concrete
// harm here — Xlib delivers protocol errors on whichever thread happens to read
// the socket, so a thread's `clear_x_error` → request → `sync` → `take_x_error`
// sequence could pick up a failure another thread provoked, and the compositor
// would answer for a request that succeeded. `Send` alone says "one thread at a
// time", which is the discipline every call site already follows; `Sync` would
// claim what the error cell and the event queue cannot support.
//
// `tests/x_error_signal.rs` checks both halves against a real server: that the
// error raised on one thread is not visible from another, and that
// `XInitThreads` reports success here.
unsafe impl Send for XDisplay {}

impl XDisplay {
    /// Wrap a raw `Display*`.
    ///
    /// # Safety
    /// `ptr` must be a live `Display*` returned by `XOpenDisplay` **and** the
    /// caller must have established Xlib's thread support first — either by
    /// calling `XInitThreads()` before any other Xlib function, or by taking
    /// the pointer from [`open_x`], which does exactly that and refuses to
    /// return a display if it could not.
    ///
    /// The second clause is not decoration. The result is a `Send` type, so it
    /// may be moved to another thread, and `Send` is justified by Xlib's
    /// internal lock: a `Display*` opened on a process where `XInitThreads` was
    /// never called is not safe to share even one-owner-at-a-time across
    /// threads, so wrapping it here would hand out a value the type system
    /// believes is safe and is not.
    ///
    /// The wrapper does not take ownership and must not be closed through
    /// [`XDisplay::close`]; it borrows a connection whose real owner is the
    /// process-wide `XDisplay`/`XConn` pair from [`open_x`].
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
    ///
    /// **This is not a barrier for X protocol errors, and nothing here may rely
    /// on it being one.** Because `open_x` hands the event queue to XCB with
    /// `XSetEventQueueOwner`, libXlib does not read protocol errors off the
    /// socket: they are left there for x11rb. Measured on this crate's own
    /// `open_x` connection, an asynchronous request that the server rejects is
    /// *not* reported to the error handler by `XFlush`, by this function, or by
    /// `XEventsQueued` — zero handler invocations, so
    /// [`take_x_error`] answers `None` after a request that certainly failed.
    /// `tests/x_error_signal.rs` pins that behaviour so it cannot change
    /// quietly.
    ///
    /// The flip side is worse: a *synchronous* Xlib request that fails on this
    /// connection desynchronises libXlib from the socket, libXlib reports an I/O
    /// error, and its default handler calls `exit(1)` — the whole window manager
    /// goes down. So do not provoke X errors through Xlib here; the compositor
    /// reads x11rb's per-request errors with `checked_void!` instead, and the
    /// GLX paths that probe with [`take_x_error`] are best-effort.
    pub fn sync(self) {
        // SAFETY: `self.0` is a live `Display*` for the whole process (the type
        // is not `Drop`, so nothing the caller can reach closes it) and
        // `XSync` takes only the display and a discard flag. It makes no
        // assumption about which side of the shared socket reads the reply.
        unsafe { XSync(self.0, 0) };
    }

    /// Explicitly close the display.
    ///
    /// # Safety
    /// Every `XCBConnection` wrapping this display's connection must already be
    /// dropped, and no GLX/Vulkan resource may still be alive.
    ///
    /// Nothing this crate returns satisfies that: [`open_x`] hands back a live
    /// `XConn` alongside the display and the two are meant to live together
    /// until exit. `close` exists for `open_x`'s own error paths, where the
    /// wrapping has not yet succeeded and there is provably no borrower left.
    pub unsafe fn close(self) {
        if !self.0.is_null() {
            // SAFETY: the caller guarantees no connection or driver resource
            // still refers to this display, and `XCloseDisplay` takes no
            // argument but the display.
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
/// for the lifetime rules that follow from that. The display is never closed
/// afterwards, which is why [`XDisplay`] is not `Drop`.
pub fn open_x() -> Result<(XDisplay, XConn, usize), String> {
    // SAFETY for the whole block: the only Xlib calls are the six below, each
    // on a `Display*` this function owns from `XOpenDisplay` onwards, and each
    // passing either a pointer to that display or the address of a live local
    // for an out-parameter. No Xlib event function is called, so XCB's
    // ownership of the queue established below is never disturbed. The
    // `Display*` is intentionally not closed on the success path — the
    // `XCBConnection` returned borrows it.
    unsafe {
        // Must be the first Xlib call in the process, and its result is
        // load-bearing rather than advisory: it is what makes `XDisplay: Send`
        // sound, so a libX11 that could not install its own lock must not
        // produce a display at all. (libX11 answers non-zero on every platform
        // it currently ships; a zero here would mean the lock allocation
        // failed, and continuing would hand out a `Send` handle onto a
        // non-thread-safe Xlib.)
        if XInitThreads() == 0 {
            return Err("XInitThreads failed: Xlib is not thread-safe on this build".into());
        }
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

        // SAFETY: `dpy` is the live display this function just opened, with
        // Xlib's thread support enabled above, and the wrapper it produces is
        // returned to the caller next to the `XConn` that borrows it — so the
        // two stay alive together and neither can be closed underneath the
        // other.
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

        /// The clear/take pair is the whole contract of the cell, and it is
        /// exercised here by writing the cell directly.
        ///
        /// The alternative is to provoke a real protocol error, and on the
        /// connection `open_x` builds that cannot reach the handler at all — the
        /// cell is never written, so every assertion about it would be about an
        /// always-empty cell and would pass no matter what `clear_x_error` and
        /// `take_x_error` did. Writing it directly is what makes these assertions
        /// about the two functions rather than about the server.
        #[test]
        fn a_stale_error_is_discarded_and_a_taken_one_reported_once(code in 1u8..=255) {
            LAST_X_ERROR.with(|c| c.set(code));
            clear_x_error();
            prop_assert_eq!(
                take_x_error(),
                None,
                "clear_x_error left a recorded error in place, so a later request \\
                 would be blamed for the previous one's failure"
            );

            LAST_X_ERROR.with(|c| c.set(code));
            prop_assert_eq!(
                take_x_error(),
                Some(code),
                "take_x_error did not report the error that was recorded"
            );
            prop_assert_eq!(
                take_x_error(),
                None,
                "take_x_error reported the same error twice, so a caller that \\
                 takes without clearing would act on a failure that is already \
                 handled"
            );
        }
    }
}
