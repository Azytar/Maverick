// maverick-gl/src/glx.rs
// Hand-written GLX 1.4 + `GLX_EXT_texture_from_pixmap` +
// `GLX_ARB_create_context` + swap-control FFI.
//
// GLX is the bridge between the X server's drawables and OpenGL: it turns the
// off-screen pixmap Composite gives us for a redirected window into a texture
// (`glXBindTexImageEXT`) with **zero copies**, and it is what gives us real
// vblank synchronisation (`glXSwapIntervalEXT` + `glXSwapBuffers`) instead of
// the current 16 ms rate cap.

use crate::dl::Lib;
use crate::xlib::{Display, XID};
use std::os::raw::{c_char, c_int, c_uint, c_ulong, c_void};

pub type GLXFBConfig = *mut c_void;
pub type GLXContext = *mut c_void;
pub type GLXDrawable = XID;
pub type GLXWindow = XID;
pub type GLXPixmap = XID;
/// Xlib's `Bool` (`int`, 0/1).
pub type Bool = c_int;

// ── fbconfig attributes (GL/glx.h) ────────────────────────────────────────────
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

// ── GLX_EXT_texture_from_pixmap ───────────────────────────────────────────────
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

// ── GLX_ARB_create_context / _profile ─────────────────────────────────────────
pub const GLX_CONTEXT_MAJOR_VERSION_ARB: c_int = 0x2091;
pub const GLX_CONTEXT_MINOR_VERSION_ARB: c_int = 0x2092;
pub const GLX_CONTEXT_PROFILE_MASK_ARB: c_int = 0x9126;
pub const GLX_CONTEXT_CORE_PROFILE_BIT_ARB: c_int = 0x0000_0001;

// ── GLX_EXT_buffer_age ───────────────────────────────────────────────────────
/// Query `glXQueryDrawable` with this attribute to learn how many frames old
/// the back buffer's contents are. `0` means "undefined" (full repaint); `1`
/// means it holds the last frame we presented, so a partial redraw is safe.
pub const GLX_BACK_BUFFER_AGE_EXT: c_int = 0x20F4;

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
                    $( $rname: unsafe {
                        std::mem::transmute::<*mut c_void, unsafe extern "C" fn($($rargty),*) $(-> $rret)?>(
                            lib.sym(stringify!($rname))?
                        )
                    }, )+
                    $( $oname: lib.sym_opt(stringify!($oname)).map(|p| unsafe {
                        std::mem::transmute::<*mut c_void, unsafe extern "C" fn($($oargty),*) $(-> $oret)?>(p)
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
        // `GLX_SGI_video_sync`: block the thread until the next retrace so the
        // frame loop can pace to the real vblank instead of a fixed timer.
        fn glXGetVideoSyncSGI(count: *mut c_uint) -> c_int;
        fn glXWaitVideoSyncSGI(divisor: c_int, remainder: c_int, count: *mut c_uint) -> c_int;
        // `GLX_EXT_buffer_age`: how many frames stale the back buffer is. Drives
        // safe partial redraw (scissor) — without it a partial clear would leave
        // garbage in the un-cleared region.
        fn glXQueryDrawable(dpy: *mut Display, draw: GLXDrawable, attribute: c_int, value: *mut c_uint) -> c_int;
    }
}

impl Glx {
    /// The server's GLX extension string for `screen`, as a Rust `String`.
    ///
    /// `pub(crate)`: it takes a raw `Display*`, so it is only safe to call from
    /// inside this crate, where the pointer provenance is known.
    pub(crate) fn extensions(&self, dpy: *mut Display, screen: c_int) -> String {
        let p = unsafe { (self.glXQueryExtensionsString)(dpy, screen) };
        if p.is_null() {
            return String::new();
        }
        unsafe { std::ffi::CStr::from_ptr(p) }
            .to_string_lossy()
            .into_owned()
    }

    /// Read one fbconfig attribute, `None` when the query fails.
    pub(crate) fn config_attrib(
        &self,
        dpy: *mut Display,
        cfg: GLXFBConfig,
        attrib: c_int,
    ) -> Option<c_int> {
        let mut v: c_int = 0;
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
