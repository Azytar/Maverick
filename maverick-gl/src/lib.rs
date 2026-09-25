//! OpenGL renderer for the compositor, on a single X socket shared with the
//! window manager.
//!
//! The WM speaks pure XCB through `x11rb`; GLX — the only way to get an OpenGL
//! context and hardware vsync on X11 — is an *Xlib* API. Two connections would
//! mean two sockets, two sequence-number spaces, two event queues, and races
//! between "the WM already destroyed this window" and "the compositor is still
//! drawing it".
//!
//! libX11 solves this: open the display with Xlib, hand the event queue over to
//! XCB with `XSetEventQueueOwner(XCB_OWNS_EVENT_QUEUE)`, fetch the underlying
//! `xcb_connection_t*` with `XGetXCBConnection`, and wrap it in
//! `x11rb::xcb_ffi::XCBConnection` with `should_drop = false`. One socket, one
//! queue, one sequence space — x11rb issues every request and reads every
//! event, GLX only ever renders.
//!
//! `libX11`/`libX11-xcb` are linked (any X11 session has them). `libGL.so.1` is
//! `dlopen`ed at runtime instead, so a machine with no GL driver still starts
//! the window manager: the load fails, [`probe`] reports it, and the caller
//! stays on the non-composited path. Everything else is hand-written
//! `extern "C"`, in the same spirit as `maverick-sys` — no binding-generator
//! crate, no GL loader.
//!
//! # Ownership
//!
//! [`XDisplay`] wraps the `Display*` and is deliberately not `Drop`. The
//! `XCBConnection` returned by [`open_x`] borrows that `Display*`'s
//! `xcb_connection_t*` with `should_drop = false`; closing the display first
//! would leave the connection dangling. Both live for the whole process and, in
//! the window manager, the connection is shared as `Rc<XConn>` between the WM
//! core and the compositor. The kernel closes the socket at exit. What this
//! crate does not own: window-management state (`maverick-core`), the X event
//! loop, or the compositor frame schedule — it owns the GL context and the
//! textures derived from X pixmaps.
//!
//! # The golden rule
//!
//! After [`open_x`], **never** call an Xlib event function (`XNextEvent`,
//! `XPending`, `XPeekEvent`, ...). XCB owns the queue; Xlib would either block
//! forever or steal events the window manager needs. Only GLX entry points and
//! x11rb are allowed. `XSync` is fine: it flushes, it does not dequeue.
//!
//! # Safety
//!
//! This crate contains `unsafe` blocks for every FFI call. The safety
//! invariants are documented on each function:
//! - `XDisplay` is not `Drop` because the `XCBConnection` borrows its
//!   `xcb_connection_t*` with `should_drop = false` (see Ownership above).
//! - `silent_error_handler` records the error code synchronously; callers must
//!   follow `clear_x_error` → request → `XSync` → `take_x_error`.
//! - GL entry points are loaded at runtime via `dlopen` and valid for the
//!   process lifetime.

pub mod dl;
pub mod gl;
pub mod glx;
pub mod renderer;
pub mod xlib;

pub use renderer::{
    Acceleration, DrawQuad, Filter, Rect, Renderer, RendererBackend, RendererInfo, ShaderId,
    Texture, TextureHandle, VisualFormat, VisualReport, VsyncMode,
};
pub use xlib::XDisplay;

use x11rb::xcb_ffi::XCBConnection;

/// The connection type the whole window manager uses.
///
/// It is `XCBConnection` rather than `RustConnection` for one reason: it can be
/// built from a `Display*`'s own `xcb_connection_t*`, which is what lets GLX
/// and the WM share a single connection.
pub type XConn = XCBConnection;

/// Open the X display and return the pieces the window manager needs.
///
/// Returns `(display, connection, screen_number)`. The `XCBConnection` borrows
/// the display's connection (`should_drop = false`): the `Display*` stays the
/// owner, so nothing here ever calls `xcb_disconnect`. Both live for the whole
/// process — see [`XDisplay`] for why that is deliberate.
pub fn open_x() -> Result<(XDisplay, XConn, usize), String> {
    unsafe {
        // Must be the FIRST Xlib call in the process: it enables locking
        // inside Xlib, which is what makes holding `Display*` across threads
        // (and the `Send` impls on `XDisplay`/`Lib`) sound. No-op on
        // already-initialised libX11; returns non-zero on success.
        xlib::XInitThreads();
        let dpy = xlib::XOpenDisplay(std::ptr::null());
        if dpy.is_null() {
            let target = std::env::var("DISPLAY").unwrap_or_else(|_| "<unset>".into());
            return Err(format!("cannot open X display (DISPLAY={target})"));
        }

        // A compositor races clients by nature (a window can die between the
        // QueryTree that listed it and the request that redirects it), so X
        // errors are routine. Xlib's default handler *exits the process*;
        // replace it before issuing a single request.
        xlib::install_silent_error_handler();

        // Hand the event queue to XCB. Must happen before any event is read.
        xlib::XSetEventQueueOwner(dpy, xlib::XCB_OWNS_EVENT_QUEUE);

        let screen = xlib::XDefaultScreen(dpy) as usize;
        let raw = xlib::XGetXCBConnection(dpy);
        if raw.is_null() {
            return Err("XGetXCBConnection returned NULL (libX11 built without XCB?)".into());
        }

        // SAFETY: `raw` is owned by `dpy`, which outlives the connection (the
        // returned `XDisplay` is not `Drop`), and `should_drop = false` keeps
        // x11rb from calling `xcb_disconnect` on someone else's connection.
        let conn = XCBConnection::from_raw_xcb_connection(raw, false)
            .map_err(|e| format!("x11rb could not wrap the xcb connection: {e}"))?;

        Ok((XDisplay::from_raw(dpy), conn, screen))
    }
}

/// Whether an OpenGL driver is present at all (`dlopen("libGL.so.1")`).
///
/// Cheap enough to call before doing any Composite setup, so a machine without
/// GL never claims `_NET_WM_CM_S0` nor redirects anything.
pub fn probe() -> bool {
    dl::Lib::open_gl().is_ok()
}

#[cfg(test)]
mod tests {
    use super::glx::has_extension;

    #[test]
    fn extension_matching_is_token_exact() {
        let s = "GLX_EXT_swap_control_tear GLX_EXT_buffer_age";
        assert!(!has_extension(s, "GLX_EXT_swap_control"));
        assert!(has_extension(s, "GLX_EXT_swap_control_tear"));
        assert!(has_extension(s, "GLX_EXT_buffer_age"));
        assert!(!has_extension(s, "GLX_EXT_texture_from_pixmap"));
        assert!(!has_extension("", "GLX_EXT_buffer_age"));
    }
}
