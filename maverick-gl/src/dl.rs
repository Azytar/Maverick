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
/// disabled (but not "un-initialised") at runtime.
pub struct Lib {
    handle: *mut c_void,
    /// `glXGetProcAddressARB` — the only correct way to resolve GL/GLX
    /// extension entry points. `dlsym` alone finds the ABI-guaranteed core
    /// symbols but not driver-provided extensions.
    get_proc: Option<unsafe extern "C" fn(*const c_uchar) -> *mut c_void>,
}

unsafe impl Send for Lib {}
// `Send` (but deliberately NOT `Sync`): every GL call in the compositor runs
// on the WM thread, and `XInitThreads` (see `open_x`) makes the underlying
// libGL/Xlib locking valid if the handle ever crosses threads during setup.
// Sharing `&Lib` across threads would need `Sync`, which is NOT granted.

/// Absolute system paths probed BEFORE the bare soname, in order. A bare
/// `dlopen("libGL.so.1")` honours `LD_LIBRARY_PATH`/`LD_PRELOAD`, so a hostile
/// environment could inject code into the WM process; absolute paths are not
/// subject to search-path hijacking. The soname stays as a last resort so
/// exotic layouts (Nix store, etc.) keep working.
const GL_CANDIDATES: &[&str] = &[
    "/usr/lib/x86_64-linux-gnu/libGL.so.1",
    "/usr/lib/aarch64-linux-gnu/libGL.so.1",
    "/usr/lib64/libGL.so.1",
    "/usr/lib/libGL.so.1",
    "libGL.so.1",
];

impl Lib {
    /// Load `libGL.so.1` and resolve `glXGetProcAddressARB`.
    pub fn open_gl() -> Result<Self, String> {
        let mut last_err = String::from("no libGL candidate tried");
        for cand in GL_CANDIDATES {
            let name = CString::new(*cand).expect("static string has no NUL");
            // RTLD_NOW: fail fast here on missing relocations instead of
            // crashing mid-frame on the first call into a half-bound driver.
            let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if handle.is_null() {
                last_err = format!("dlopen({cand}) failed: {}", last_error());
                continue;
            }
            let mut lib = Lib {
                handle,
                get_proc: None,
            };
            let raw = lib.dlsym("glXGetProcAddressARB");
            if raw.is_null() {
                last_err = format!("{cand} has no glXGetProcAddressARB");
                continue;
            }
            lib.get_proc = Some(unsafe { Self::cast_fn(raw) });
            return Ok(lib);
        }
        Err(last_err)
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

    /// Cast a resolved, non-null symbol to a typed entry point. Single choke
    /// point for every `transmute` in the crate (`gl.rs`/`glx.rs` go through
    /// here), so the safety contract lives in one place.
    ///
    /// # Safety
    /// `p` must be a non-null pointer to a function with signature `T`,
    /// obtained from [`Lib::sym`] (i.e. from `glXGetProcAddressARB`/`dlsym`
    /// for exactly `name`). A driver returning a non-null *stub* for a
    /// missing extension would still be UB to call — which is why callers
    /// must treat optional symbols via `sym_opt` + extension-string checks,
    /// never by nullness alone.
    pub unsafe fn cast_fn<T>(p: *mut c_void) -> T {
        debug_assert!(!p.is_null());
        unsafe { std::mem::transmute_copy::<*mut c_void, T>(&p) }
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

/// The two properties of the loader that hold without a driver being present:
/// the candidate list keeps the search path it is supposed to keep, and a
/// symbol name that cannot be one is never resolved.
#[cfg(test)]
mod property_tests {
    use super::*;

    /// Every candidate is an absolute path except the bare soname, which is
    /// there only as a last resort, tried last and exactly once.
    ///
    /// This is the crate's one piece of hardening: a bare `dlopen("libGL.so.1")`
    /// honours `LD_LIBRARY_PATH` and `LD_PRELOAD`, so a hostile environment in
    /// the compositor's process could inject code into it, while an absolute
    /// path cannot. The soname stays last so a layout the fixed list does not
    /// know (a Nix store, say) keeps working. A NUL inside a candidate would
    /// panic the loader's `CString::new(..).expect` on a machine nobody
    /// expects to fail.
    ///
    /// The list is a constant, so this is a scan of it rather than a generated
    /// case.
    #[test]
    fn gl_candidates_are_absolute_paths_first_and_nul_free() {
        let (last, head) = GL_CANDIDATES.split_last().expect("a candidate list");
        assert_eq!(*last, "libGL.so.1", "the bare soname is the last resort");
        for cand in head {
            assert!(
                cand.starts_with('/'),
                "{cand} is not absolute, so a search path could hijack it"
            );
        }
        for cand in GL_CANDIDATES {
            assert!(!cand.contains('\0'), "{cand:?} is not a C string");
            assert!(cand.ends_with("libGL.so.1"), "{cand} is not libGL at all");
        }
        for (i, cand) in GL_CANDIDATES.iter().enumerate() {
            assert_eq!(
                GL_CANDIDATES.iter().filter(|c| *c == cand).count(),
                1,
                "{cand} is tried more than once, so a failing one fails twice"
            );
            let _ = i;
        }
    }

    /// A symbol name carrying an interior NUL resolves to nothing, even when
    /// its prefix is a symbol the driver really has.
    ///
    /// The lookup goes through `glXGetProcAddressARB`/`dlsym`, both of which
    /// take a C string: handed a name with a NUL in it, the C side would stop
    /// there and resolve the *prefix*. That turns "look up this name" into
    /// "look up whatever the caller started with", which is how a caller ends
    /// up calling the wrong entry point.
    ///
    /// Needs a driver to look anything up; on a machine with none there is
    /// nothing to resolve against and the guard is unreachable, so the check
    /// is simply not made there.
    #[test]
    fn a_symbol_name_with_an_embedded_nul_never_resolves() {
        let Ok(lib) = Lib::open_gl() else {
            return;
        };
        for name in [
            "glGetError\0junk",
            "\0",
            "glXSwapBuffers\0",
            "junk\0glGetError",
        ] {
            assert!(
                lib.sym_opt(name).is_none(),
                "{name:?} resolved to something"
            );
            let err = lib
                .sym(name)
                .expect_err("a name with a NUL must not resolve");
            assert!(
                err.contains(name),
                "{err:?} does not name the symbol it missed"
            );
        }
    }
}
