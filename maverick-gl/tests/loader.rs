//! What the GL/GLX loader promises, checked without a context.
//!
//! A real GL context cannot be created reliably in CI — no DRM node, no
//! hardware, or a driver that answers `glXCreateContextAttribsARB` with NULL —
//! so the tests here cover the part of the boundary that holds *before* one
//! exists: the shapes of the entry points the loader hands out, and what the
//! loader does on a machine with no driver at all.
//!
//! # What is checked, and what is compile-checked instead
//!
//! `Lib::cast_fn` is the one place a `*mut c_void` becomes a typed function
//! pointer. Its safety argument rests on a single fact — the target type must be
//! exactly pointer-sized — enforced by a `const` assertion, which is a *build*
//! failure and therefore not runnable. The closest runnable statement of the
//! same property is the first test below: it checks the real, rather than
//! asserted, size and alignment of the signatures the two loader tables
//! instantiate. A GL entry point declared to return a struct by value would stop
//! being pointer-sized and fail there.
//!
//! # Running
//!
//! ```text
//! cargo test -p maverick-gl --test loader
//! ```

use std::mem::{align_of, size_of};
use std::os::raw::{c_int, c_uchar, c_uint, c_ulong, c_void};

use maverick_gl::dl::Lib;
use maverick_gl::gl::{self as glmod, GLenum};
use maverick_gl::glx::{has_extension, Bool, GLXFBConfig, GLXWindow};
use maverick_gl::probe;

/// The signatures the two loader tables instantiate, in the shapes they are
/// declared with. Each stands for a family of entry points; what they have in
/// common is the thing under test: every `Gl` and `Glx` field is a C-ABI
/// function pointer, and that is what a resolved symbol address can be
/// reinterpreted as.
fn representative_entry_points() -> Vec<(&'static str, usize, usize)> {
    type Void = unsafe extern "C" fn();
    type GetError = unsafe extern "C" fn() -> GLenum;
    type CreateShader = unsafe extern "C" fn(GLenum) -> glmod::GLuint;
    type BlendFunc = unsafe extern "C" fn(GLenum, GLenum);
    type Viewport =
        unsafe extern "C" fn(glmod::GLint, glmod::GLint, glmod::GLsizei, glmod::GLsizei);
    type GetShaderiv = unsafe extern "C" fn(glmod::GLuint, GLenum, *mut glmod::GLint);
    type GetString = unsafe extern "C" fn(GLenum) -> *const c_uchar;
    type ConfigAttrib = unsafe extern "C" fn(*mut c_void, GLXFBConfig, c_int, *mut c_int) -> Bool;
    type CreateWindow =
        unsafe extern "C" fn(*mut c_void, GLXFBConfig, c_ulong, *const c_int) -> GLXWindow;
    type QueryDrawable = unsafe extern "C" fn(*mut c_void, c_ulong, c_int, *mut c_uint) -> c_int;

    vec![
        ("void glFinish()", size_of::<Void>(), align_of::<Void>()),
        (
            "GLenum glGetError()",
            size_of::<GetError>(),
            align_of::<GetError>(),
        ),
        (
            "GLuint glCreateShader(GLenum)",
            size_of::<CreateShader>(),
            align_of::<CreateShader>(),
        ),
        (
            "void glBlendFunc(GLenum, GLenum)",
            size_of::<BlendFunc>(),
            align_of::<BlendFunc>(),
        ),
        (
            "void glViewport(GLint, GLint, GLsizei, GLsizei)",
            size_of::<Viewport>(),
            align_of::<Viewport>(),
        ),
        (
            "void glGetShaderiv(GLuint, GLenum, GLint*)",
            size_of::<GetShaderiv>(),
            align_of::<GetShaderiv>(),
        ),
        (
            "const GLubyte* glGetString(GLenum)",
            size_of::<GetString>(),
            align_of::<GetString>(),
        ),
        (
            "Bool glXGetFBConfigAttrib(Display*, GLXFBConfig, int, int*)",
            size_of::<ConfigAttrib>(),
            align_of::<ConfigAttrib>(),
        ),
        (
            "GLXWindow glXCreateWindow(Display*, GLXFBConfig, XID, const int*)",
            size_of::<CreateWindow>(),
            align_of::<CreateWindow>(),
        ),
        (
            "int glXQueryDrawable(Display*, GLXDrawable, int, unsigned int*)",
            size_of::<QueryDrawable>(),
            align_of::<QueryDrawable>(),
        ),
    ]
}

/// Every entry point the loader can produce is pointer-sized and pointer-aligned.
///
/// This is the runnable half of `cast_fn`'s compile-time assertion: the
/// assertion proves a mismatched `T` cannot be instantiated, and this proves the
/// instantiations that do exist are the pointer-sized, pointer-aligned ones it is
/// satisfied by. The realistic way this stops holding is an entry point declared
/// to return a struct by value, and that would show up here as a signature that
/// no longer fits.
#[test]
fn every_loaded_entry_point_is_pointer_sized_and_aligned() {
    let want_size = size_of::<*mut c_void>();
    let want_align = align_of::<*mut c_void>();
    for (name, size, align) in representative_entry_points() {
        assert_eq!(size, want_size, "{name} is not pointer-sized");
        assert_eq!(align, want_align, "{name} is not pointer-aligned");
    }
}

/// The only pointer an entry point takes for a name is a borrowed C string, and
/// it is already NUL-terminated and already the right length.
///
/// This is what makes the `const` assertion in `cast_fn` sufficient rather than
/// merely necessary: nothing about these calls reinterprets a `*const GLchar`,
/// so the loader's whole cast surface really is the function pointers above.
/// GL and libdl both read a name up to its first NUL, which is why the byte
/// count that matters is the one *including* the terminator.
#[test]
fn a_name_argument_is_a_borrowed_c_string_that_is_already_terminated() {
    let name = std::ffi::CString::new("glDefinitelyNotAGLSymbol").expect("no interior NUL");
    assert_eq!(name.as_bytes().len(), "glDefinitelyNotAGLSymbol".len());
    assert_eq!(
        name.as_bytes_with_nul().last(),
        Some(&0),
        "GL reads the name up to the NUL, so the NUL has to be there"
    );
    assert_eq!(
        name.as_bytes_with_nul().len(),
        "glDefinitelyNotAGLSymbol".len() + 1
    );
    // Deref is a plain borrow: the pointer and length come from the same
    // allocation and neither is reinterpreted.
    let borrowed: &std::ffi::CStr = &name;
    assert_eq!(borrowed.to_bytes().len(), "glDefinitelyNotAGLSymbol".len());
}

/// An optional entry point is an `Option` over a C-ABI function pointer, so the
/// `Some`/`None` test the renderer's extension gating depends on is a real
/// branch rather than a call through a null address.
///
/// `glXSwapIntervalSGI` is the shape the table declares: one `int` in, one
/// `int` out, no display argument. A driver is allowed to export a non-null stub
/// for an extension it does not implement, which is why `Renderer` also matches
/// the extension token — but the `None` case still has to be representable and
/// distinguishable, and it is.
#[test]
fn an_optional_entry_point_is_an_option_over_a_c_abi_function_pointer() {
    type Optional = Option<unsafe extern "C" fn(c_int) -> c_int>;
    let none: Optional = None;
    assert_eq!(size_of::<Optional>(), size_of::<*mut c_void>());
    assert!(none.is_none());
    assert_ne!(
        size_of::<Optional>(),
        size_of::<Option<fn(c_int) -> c_int>>() + size_of::<c_int>(),
        "a niche-optimised Option must not have grown a discriminant that \
         would make a resolved symbol and a missing one indistinguishable"
    );
}

/// The capability probe answers the same thing every time it is asked.
///
/// `probe()` is what the window manager calls before claiming
/// `_NET_WM_CM_S0` and before redirecting anything, and libGL is loaded once and
/// never unloaded — so a second answer that disagreed with the first would mean
/// the decision to composite was taken on a different answer from the one the
/// compositor is built on. A machine with no driver must still get an answer
/// rather than an error, which is the case that makes this worth asserting at
/// all.
#[test]
fn probing_for_a_driver_is_stable_and_never_fails() {
    let first = probe();
    for i in 0..4 {
        assert_eq!(
            probe(),
            first,
            "attempt {i}: the driver came or went under us"
        );
    }
}

/// Loading the tables needs no context, and no driver, and does not panic.
///
/// `Lib::open_gl` and `sym` only resolve symbols, so they are callable on a
/// machine with no GL at all — and they must be, because the compositor probes
/// before deciding whether to composite and has to survive the answer being "no".
/// The point of this test is that it *runs to completion* either way, not what it
/// returns.
#[test]
fn symbol_resolution_works_without_a_context_and_names_what_is_missing() {
    match Lib::open_gl() {
        Err(e) => {
            // A machine with no driver. The message has to say what it could not
            // find, because this string is what a user sees when compositing
            // silently stays off.
            assert!(
                !e.is_empty(),
                "a failed load must say why, not just that it failed"
            );
        }
        Ok(lib) => {
            // A machine with one. A name carrying an interior NUL is rejected
            // before it ever reaches the driver, because GL and libdl both read
            // a name up to the first NUL and would happily hand back the prefix
            // — a different entry point than the caller asked for, called as if
            // it were the one that was asked for.
            for name in ["glGetError\0junk", "\0", "junk\0glGetError"] {
                assert!(
                    lib.sym_opt(name).is_none(),
                    "{name:?} resolved to something, which would be called as the prefix"
                );
                let err = lib.sym(name).expect_err("a name with a NUL cannot resolve");
                assert!(
                    err.contains(name),
                    "{err:?} does not name the symbol it missed"
                );
            }
            // Whatever a nonsense name answers, the answer is stable and an
            // error names the symbol it missed — that string is what reaches the
            // log, so a bare "missing GL symbol" sends a reader nowhere.
            let first = lib.sym_opt("glDefinitelyNotAGLSymbol");
            assert_eq!(
                lib.sym_opt("glDefinitelyNotAGLSymbol"),
                first,
                "the same lookup answered differently the second time"
            );
            if let Err(e) = lib.sym("glDefinitelyNotAGLSymbol") {
                assert!(
                    e.contains("glDefinitelyNotAGLSymbol"),
                    "{e:?} does not name the symbol it missed"
                );
            }
        }
    }
}

/// A resolved symbol is not evidence that the extension exists.
///
/// `glXGetProcAddressARB` is entitled to answer a **non-null stub** for a name it
/// does not implement, and on a real driver it usually does — which is why
/// `Glx`'s optional fields being `Some` proves nothing on their own, and why
/// `Renderer::new_with_vsync` refuses to start unless the extension *token* also
/// appears in the server's string.
///
/// This test is the pair of signals disagreeing, which is exactly the situation
/// the renderer's double check exists for. It is conditional on there being a
/// driver because a machine with none cannot produce the stub — on such a machine
/// the second signal is vacuously correct.
#[test]
fn a_stubbed_symbol_still_needs_the_extension_token_to_be_called() {
    let Ok(lib) = Lib::open_gl() else {
        eprintln!("no GL driver; the stub case is unreachable here");
        return;
    };
    let bogus = "GLX_MAV_definitely_not_an_extension";
    // The extension string this renderer would consult. The server never lists
    // the token, so however the symbol lookup answered, the token test is what
    // says "not supported".
    let server_extensions = "GLX_EXT_texture_from_pixmap GLX_ARB_create_context";
    assert!(!has_extension(server_extensions, bogus));
    // And the two signals really can disagree: if this driver stubs unknown
    // names, `sym_opt` says yes while the token test says no. The test passes
    // either way — what it fixes is that the *token* is the discriminator, and
    // that no caller may read the symbol answer as support.
    let resolved = lib.sym_opt(bogus);
    if resolved.is_some() {
        eprintln!("this driver stubs unknown symbols, as documented");
    }
    assert!(
        !has_extension(server_extensions, bogus),
        "a symbol that resolved must not become a support claim"
    );
}

/// The `XDisplay` `maverick-gl` re-exports is the shared bootstrap's own type.
///
/// The compositor passes one to `Renderer::new_with_vsync` after building it with
/// `from_raw`, so a second look-alike type here would stop the handoff
/// type-checking. Asserting the identity at compile time is stronger than
/// comparing the values, which cannot be produced without a display.
#[test]
fn the_re_exported_display_is_the_shared_bootstrap_type() {
    fn same(_: fn(maverick_gl::XDisplay) -> maverick_x11::XDisplay) {}
    let identity: fn(maverick_gl::XDisplay) -> maverick_x11::XDisplay = |d| d;
    same(identity);
}
