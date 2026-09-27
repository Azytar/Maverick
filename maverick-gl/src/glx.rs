// Hand-written GLX 1.4 + `GLX_EXT_texture_from_pixmap` +
// `GLX_ARB_create_context` + swap-control FFI.
//
// GLX is the bridge between the X server's drawables and OpenGL: it turns the
// off-screen pixmap Composite gives us for a redirected window into a texture
// (`glXBindTexImageEXT`) with **zero copies**, and it is what gives us real
// vblank synchronisation (`glXSwapBuffers` at swap interval 1) instead of a
// fixed sleep between frames.

use crate::dl::Lib;
use maverick_x11::{Display, XID};
use std::os::raw::{c_char, c_int, c_uint, c_ulong, c_void};

pub type GLXFBConfig = *mut c_void;
pub type GLXContext = *mut c_void;
pub type GLXDrawable = XID;
pub type GLXWindow = XID;
pub type GLXPixmap = XID;
/// Xlib's `Bool` (`int`, 0/1).
pub type Bool = c_int;

pub const GLX_BUFFER_SIZE: c_int = 2;
pub const GLX_DOUBLEBUFFER: c_int = 5;
pub const GLX_RED_SIZE: c_int = 8;
pub const GLX_GREEN_SIZE: c_int = 9;
pub const GLX_BLUE_SIZE: c_int = 10;
pub const GLX_ALPHA_SIZE: c_int = 11;
pub const GLX_DEPTH_SIZE: c_int = 12;
pub const GLX_STENCIL_SIZE: c_int = 13;
pub const GLX_CONFIG_CAVEAT: c_int = 0x20;
pub const GLX_VISUAL_ID: c_int = 0x800B;
pub const GLX_DRAWABLE_TYPE: c_int = 0x8010;
pub const GLX_RENDER_TYPE: c_int = 0x8011;
pub const GLX_X_RENDERABLE: c_int = 0x8012;
pub const GLX_RGBA_TYPE: c_int = 0x8014;
pub const GLX_NONE: c_int = 0x8000;
pub const GLX_DONT_CARE: c_int = -1;

pub const GLX_WINDOW_BIT: c_int = 0x0000_0001;
pub const GLX_PIXMAP_BIT: c_int = 0x0000_0002;
pub const GLX_RGBA_BIT: c_int = 0x0000_0001;

pub const GLX_BIND_TO_TEXTURE_RGB_EXT: c_int = 0x20D0;
pub const GLX_BIND_TO_TEXTURE_RGBA_EXT: c_int = 0x20D1;
pub const GLX_BIND_TO_TEXTURE_TARGETS_EXT: c_int = 0x20D3;
pub const GLX_Y_INVERTED_EXT: c_int = 0x20D4;
pub const GLX_TEXTURE_FORMAT_EXT: c_int = 0x20D5;
pub const GLX_TEXTURE_TARGET_EXT: c_int = 0x20D6;
pub const GLX_TEXTURE_FORMAT_NONE_EXT: c_int = 0x20D8;
pub const GLX_TEXTURE_FORMAT_RGB_EXT: c_int = 0x20D9;
pub const GLX_TEXTURE_FORMAT_RGBA_EXT: c_int = 0x20DA;
pub const GLX_TEXTURE_2D_BIT_EXT: c_int = 0x0000_0002;
pub const GLX_TEXTURE_2D_EXT: c_int = 0x20DC;
pub const GLX_FRONT_LEFT_EXT: c_int = 0x20DE;

pub const GLX_CONTEXT_MAJOR_VERSION_ARB: c_int = 0x2091;
pub const GLX_CONTEXT_MINOR_VERSION_ARB: c_int = 0x2092;
pub const GLX_CONTEXT_PROFILE_MASK_ARB: c_int = 0x9126;
pub const GLX_CONTEXT_CORE_PROFILE_BIT_ARB: c_int = 0x0000_0001;

/// Query `glXQueryDrawable` with this attribute to learn how many frames old
/// the back buffer's contents are. `0` means "undefined" (full repaint); `1`
/// means it holds the last frame we presented, so a partial redraw is safe.
pub const GLX_BACK_BUFFER_AGE_EXT: c_int = 0x20F4;

/// Declares the `Glx` struct (one field per entry point) plus its loader.
///
/// `required` entry points fail `load` when the symbol is missing, because the
/// compositor cannot run without them. `optional` entry points are extension
/// functions and load to `None`; `None` is *not* proof that the extension is
/// absent — a driver is free to export a non-null stub for something it does
/// not implement — so callers must additionally match the extension token
/// against `glXQueryExtensionsString` (see [`has_extension`]) before using one.
///
/// As in `gl.rs`, the `unsafe` the macro emits is only `Lib::cast_fn`'s: 24
/// entry points, one contract, written down once. The invariant this table
/// cannot state is the one that matters most — a GLX call needs the caller's
/// context current on the caller's thread, and that belongs to `Renderer`,
/// which is `!Send` because it holds the context as a raw pointer.
macro_rules! glx_api {
    (
        required { $( fn $rname:ident ( $($rarg:ident : $rargty:ty),* $(,)? ) $(-> $rret:ty)? ; )+ }
        optional { $( fn $oname:ident ( $($oarg:ident : $oargty:ty),* $(,)? ) $(-> $oret:ty)? ; )+ }
    ) => {
        #[allow(non_snake_case)]
        pub struct Glx {
            $( pub $rname: unsafe extern "C" fn($($rargty),*) $(-> $rret)?, )+
            $( pub $oname: Option<unsafe extern "C" fn($($oargty),*) $(-> $oret)?>, )+
        }

        impl Glx {
            pub fn load(lib: &Lib) -> Result<Self, String> {
                Ok(Self {
                    // SAFETY: a required entry point was resolved from `lib`'s
                    // own mapping under exactly its own name, so `cast_fn`'s
                    // contract — a pointer to a function of the signature written
                    // beside it, which is this field's type — holds by
                    // construction rather than by review.
                    $( $rname: unsafe {
                        Lib::cast_fn(lib.sym(stringify!($rname))?)
                    }, )+
                    // SAFETY: as above for the optional half. `sym_opt` may
                    // answer a non-null *stub* for something the driver does not
                    // implement, so this `Some` is not a promise the entry point
                    // is callable — the extension-string check in
                    // `has_extension` is, and `Renderer::new_with_vsync` refuses
                    // to start without it.
                    $( $oname: lib.sym_opt(stringify!($oname)).map(|p| unsafe {
                        Lib::cast_fn(p)
                    }), )+
                })
            }
        }
    };
}

glx_api! {
    required {
        fn glXQueryExtension(dpy: *mut Display, error_base: *mut c_int, event_base: *mut c_int) -> Bool;
        fn glXQueryVersion(dpy: *mut Display, major: *mut c_int, minor: *mut c_int) -> Bool;
        fn glXQueryExtensionsString(dpy: *mut Display, screen: c_int) -> *const c_char;
        fn glXGetFBConfigs(dpy: *mut Display, screen: c_int, nelements: *mut c_int) -> *mut GLXFBConfig;
        fn glXChooseFBConfig(dpy: *mut Display, screen: c_int, attribs: *const c_int, nitems: *mut c_int) -> *mut GLXFBConfig;
        fn glXGetFBConfigAttrib(dpy: *mut Display, cfg: GLXFBConfig, attrib: c_int, value: *mut c_int) -> c_int;
        fn glXCreateWindow(dpy: *mut Display, cfg: GLXFBConfig, win: c_ulong, attribs: *const c_int) -> GLXWindow;
        fn glXDestroyWindow(dpy: *mut Display, win: GLXWindow);
        fn glXCreatePixmap(dpy: *mut Display, cfg: GLXFBConfig, pixmap: c_ulong, attribs: *const c_int) -> GLXPixmap;
        fn glXDestroyPixmap(dpy: *mut Display, pixmap: GLXPixmap);
        fn glXCreateNewContext(dpy: *mut Display, cfg: GLXFBConfig, render_type: c_int, share: GLXContext, direct: Bool) -> GLXContext;
        fn glXDestroyContext(dpy: *mut Display, ctx: GLXContext);
        fn glXMakeCurrent(dpy: *mut Display, drawable: GLXDrawable, ctx: GLXContext) -> Bool;
        fn glXSwapBuffers(dpy: *mut Display, drawable: GLXDrawable);
        fn glXIsDirect(dpy: *mut Display, ctx: GLXContext) -> Bool;
    }
    optional {
        fn glXCreateContextAttribsARB(dpy: *mut Display, cfg: GLXFBConfig, share: GLXContext, direct: Bool, attribs: *const c_int) -> GLXContext;
        fn glXBindTexImageEXT(dpy: *mut Display, drawable: GLXDrawable, buffer: c_int, attribs: *const c_int);
        fn glXReleaseTexImageEXT(dpy: *mut Display, drawable: GLXDrawable, buffer: c_int);
        fn glXSwapIntervalEXT(dpy: *mut Display, drawable: GLXDrawable, interval: c_int);
        fn glXSwapIntervalMESA(interval: c_uint) -> c_int;
        fn glXSwapIntervalSGI(interval: c_int) -> c_int;
        // `GLX_SGI_video_sync`: block the thread until the next retrace, so a
        // caller can pace to the real vblank instead of a fixed timer.
        fn glXGetVideoSyncSGI(count: *mut c_uint) -> c_int;
        fn glXWaitVideoSyncSGI(divisor: c_int, remainder: c_int, count: *mut c_uint) -> c_int;
        // `GLX_EXT_buffer_age`: how many frames stale the back buffer is. Drives
        // safe partial redraw (scissor) — without it a partial clear would leave
        // garbage in the un-cleared region.
        fn glXQueryDrawable(dpy: *mut Display, draw: GLXDrawable, attribute: c_int, value: *mut c_uint) -> c_int;
    }
}

impl Glx {
    /// The server's GLX extension string for `screen`, as a Rust `String`
    /// (empty when the server answers NULL, which makes every token test fail
    /// and therefore disables every optional path).
    ///
    /// The returned storage belongs to the GLX client library — the driver
    /// builds this string once per screen and hands out the same pointer on
    /// every call — so it is **not** `XFree`d here. Freeing it would hand
    /// `XFree` a pointer Xlib never allocated. What the caller does get is a
    /// copy, so nothing in the returned `String` refers back into the driver.
    pub(crate) fn extensions(&self, dpy: *mut Display, screen: c_int) -> String {
        // SAFETY: `dpy` is a live `Display*` (see `Renderer::dpy`) and
        // `screen` came from `open_x`'s `XDefaultScreen` or the same source, so
        // it names a screen of this connection. The query needs no current
        // context, and it is a client-library call, so a context being current
        // elsewhere changes nothing.
        let p = unsafe { (self.glXQueryExtensionsString)(dpy, screen) };
        if p.is_null() {
            return String::new();
        }
        // SAFETY: a non-null extension string is NUL-terminated by the GLX
        // spec, and it stays valid for as long as the screen does — longer
        // than the copy below, which is the first thing that happens after the
        // call. `CStr::from_ptr` reads only up to that terminator.
        unsafe { std::ffi::CStr::from_ptr(p) }
            .to_string_lossy()
            .into_owned()
    }

    /// Read one fbconfig attribute, `None` when the query fails.
    ///
    /// `cfg` must be a `GLXFBConfig` this display's `glXGetFBConfigs`
    /// returned. A handle stays valid for the life of the screen even after the
    /// array it arrived in has been `XFree`d — the server owns the
    /// configuration, the array is only a client-side index into it — which is
    /// what lets `choose_window_fbconfig` and `choose_tfp_fbconfig` return
    /// their pick after freeing the list.
    pub(crate) fn config_attrib(
        &self,
        dpy: *mut Display,
        cfg: GLXFBConfig,
        attrib: c_int,
    ) -> Option<c_int> {
        let mut v: c_int = 0;
        // SAFETY: `dpy` is live, `cfg` is a config of `dpy`'s screen per the
        // contract above, and `&mut v` is the address of a live local that
        // `glXGetFBConfigAttrib` writes exactly one `int` into. The call takes
        // no ownership and allocates nothing.
        let rc = unsafe { (self.glXGetFBConfigAttrib)(dpy, cfg, attrib, &mut v) };
        if rc == 0 {
            Some(v)
        } else {
            None
        }
    }
}

/// True when `needle` appears as a whole, space-delimited token of `haystack`.
/// `"GLX_EXT_swap_control"` must not match `"GLX_EXT_swap_control_tear"`.
pub fn has_extension(haystack: &str, needle: &str) -> bool {
    haystack.split_whitespace().any(|t| t == needle)
}
