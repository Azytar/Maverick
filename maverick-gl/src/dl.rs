// maverick-gl/src/dl.rs
// Minimal `dlopen`/`dlsym` wrapper.
//
// `libGL.so.1` is loaded at *runtime*, never linked, so `maverick` keeps
// starting on a machine with no GL driver at all (a VM, a broken Mesa install,
// an `LD_PRELOAD` that hides libGL): the load simply fails, `probe()` reports
// it, and the window manager falls back to the plain `ConfigureWindow` path.
// libX11/libX11-xcb are a different story — they are a hard dependency of any
// X11 session, so those are linked normally (see `xlib.rs`).

use std::ffi::CString;
use std::os::raw::{c_char, c_uchar, c_void};

/// A `dlopen`ed shared object.
///
/// Never closed: GL function pointers, the GLX context and every texture we
/// created stay valid only while libGL is mapped, and the compositor can be
/// disabled (but not "un-initialised") at runtime. The handle lives for the
/// process, which is exactly the lifetime we want.
pub struct Lib {
    handle: *mut c_void,
    /// `glXGetProcAddressARB` — the only correct way to resolve GL/GLX
    /// extension entry points. `dlsym` alone finds the ABI-guaranteed core
    /// symbols but not driver-provided extensions.
    get_proc: Option<unsafe extern "C" fn(*const c_uchar) -> *mut c_void>,
}

unsafe impl Send for Lib {}

impl Lib {
    /// Load `libGL.so.1` and resolve `glXGetProcAddressARB`.
    pub fn open_gl() -> Result<Self, String> {
        let name = CString::new("libGL.so.1").expect("static string has no NUL");
        // RTLD_LAZY: we only ever call symbols we successfully resolved, and a
        // lazy load avoids paying for every relocation in the driver.
        let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
        if handle.is_null() {
            return Err(format!("dlopen(libGL.so.1) failed: {}", last_error()));
        }
        let mut lib = Lib {
            handle,
            get_proc: None,
        };
        let raw = lib.dlsym("glXGetProcAddressARB");
        if raw.is_null() {
            return Err("libGL.so.1 has no glXGetProcAddressARB".into());
        }
        lib.get_proc = Some(unsafe {
            std::mem::transmute::<*mut c_void, unsafe extern "C" fn(*const c_uchar) -> *mut c_void>(
                raw,
            )
        });
        Ok(lib)
    }

    fn dlsym(&self, name: &str) -> *mut c_void {
        let Ok(c) = CString::new(name) else {
            return std::ptr::null_mut();
        };
        unsafe { libc::dlsym(self.handle, c.as_ptr()) }
    }

    /// Resolve `name`, trying `glXGetProcAddressARB` first and falling back to
    /// `dlsym`. Returns `Err` with the symbol name when both fail so the caller
    /// can report exactly which entry point the driver is missing.
    pub fn sym(&self, name: &str) -> Result<*mut c_void, String> {
        match self.sym_opt(name) {
            Some(p) => Ok(p),
            None => Err(format!("missing GL symbol: {name}")),
        }
    }

    /// Like [`Lib::sym`] but `None` instead of an error — for optional
    /// extension entry points (`glXSwapIntervalEXT`, ...).
    pub fn sym_opt(&self, name: &str) -> Option<*mut c_void> {
        if let Some(get_proc) = self.get_proc {
            let Ok(c) = CString::new(name) else {
                return None;
            };
            let p = unsafe { get_proc(c.as_ptr().cast::<c_uchar>()) };
            if !p.is_null() {
                return Some(p);
            }
        }
        let p = self.dlsym(name);
        if p.is_null() {
            None
        } else {
            Some(p)
        }
    }
}

fn last_error() -> String {
    let e: *mut c_char = unsafe { libc::dlerror() };
    if e.is_null() {
        return "unknown error".into();
    }
    unsafe { std::ffi::CStr::from_ptr(e) }
        .to_string_lossy()
        .into_owned()
}
