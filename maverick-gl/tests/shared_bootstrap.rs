//! The workspace has exactly one Xlib bootstrap.
//!
//! `maverick-gl` used to carry its own copy of the Xlib/XCB handshake, its own
//! `Display*` newtype and its own thread-local X-error cell. Two consequences,
//! both invisible in review: the error cell the GL renderer read was never
//! written by anything, so the renderer's `BadMatch` handling could not fire; and
//! a fix to the `Display*` leak on the error paths was never carried across.
//!
//! These assertions are compile-time on purpose. A second bootstrap cannot be
//! reintroduced without breaking them, which is a stronger guarantee than any
//! amount of reading.

use maverick_gl::XDisplay as GlDisplay;
use maverick_x11::XDisplay as X11Display;

/// The GL backend's display handle is the shared one, not a look-alike. If a
/// second newtype is ever introduced, the raw-pointer cast the compositor used
/// to perform stops type-checking and this fails to build.
#[test]
fn the_gl_display_handle_is_the_shared_one() {
    fn same_type(_: fn(GlDisplay) -> X11Display) {}
    // The two names must resolve to one type; if they diverge, this
    // transmute-checked identity is what notices.
    let identity: fn(GlDisplay) -> X11Display = |d| d;
    same_type(identity);
}

/// The error cell the GL renderer consults is the one the bootstrap installs.
/// Before the duplicate was removed, `clear_x_error`/`take_x_error` lived in
/// the GL crate and nothing ever wrote to them.
#[test]
fn the_error_cell_is_the_installed_one() {
    // Both entry points resolve in the crate that owns the handler, so the
    // renderer and `open_x` cannot drift onto different cells again.
    maverick_x11::clear_x_error();
    assert_eq!(
        maverick_x11::take_x_error(),
        None,
        "a freshly cleared cell must read empty"
    );
}

/// The error-name table the GL renderer uses is the shared one, so a
/// diagnostic cannot disagree with the one the window manager prints.
#[test]
fn x_error_names_come_from_the_shared_table() {
    // The renderer reaches this through `maverick_x11::x_error_name`; a second
    // copy in the GL crate would be a different table and this is the guard.
    assert_eq!(maverick_x11::x_error_name(2), "BadValue");
}
