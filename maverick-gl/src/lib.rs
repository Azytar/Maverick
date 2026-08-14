// maverick-gl/src/lib.rs
//
// The X-connection bootstrap and the OpenGL renderer behind Maverick's
// compositor.
//
// ## Why this crate exists
//
// Maverick's window manager speaks pure XCB through `x11rb`. GLX, the only way
// to get an OpenGL context and hardware vsync on X11, is an *Xlib* API. Running
// two connections would mean two sequence-number spaces, two event queues and
// races between "the WM already destroyed this window" and "the compositor is
// still drawing it".
//
// libX11 solves this exact problem: open the display with Xlib, hand the event
// queue over to XCB with `XSetEventQueueOwner`, fetch the underlying
// `xcb_connection_t*` with `XGetXCBConnection`, and wrap it in
// `x11rb::xcb_ffi::XCBConnection`. One socket, one queue, one sequence space —
// x11rb issues every request and reads every event, GLX only ever renders.
//
// ## The golden rule
//
// After [`open_x`], **never** call an Xlib event function (`XNextEvent`,
// `XPending`, `XPeekEvent`, ...). XCB owns the queue; Xlib would either block
// forever or steal events the window manager needs. Only GLX entry points and
// x11rb are allowed. `XSync` is fine (it flushes, it does not dequeue).
//
// ## Zero new third-party crates
//
// Everything below is hand-written `extern "C"`, in the same spirit as
// `maverick-sys`. `libX11`/`libX11-xcb` are linked (any X11 session has them);
// `libGL.so.1` is `dlopen`ed at runtime, so a machine with no GL driver still
// runs the window manager — it just falls back to the non-composited path.

pub mod dl;
pub mod gl;
pub mod glx;
pub mod renderer;
pub mod xlib;

pub use renderer::{Quad, Rect, Renderer, Texture, VisualFormat, VisualReport};
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
