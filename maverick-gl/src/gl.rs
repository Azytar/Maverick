// Hand-written OpenGL 3.3 core FFI — only the entry points the compositor
// actually calls, resolved at runtime through `glXGetProcAddressARB`.
//
// There is no `gl` / `glow` / `gl_generator` dependency on purpose: the whole
// surface a single-quad, single-program compositor needs is a few dozen
// functions, and writing them out keeps the dependency set unchanged. Every
// declaration is core-profile 3.3; anything extension-based reaches GL through
// GLX (`glx.rs`), not here.

use crate::dl::Lib;
use std::os::raw::{c_char, c_float, c_int, c_uchar, c_uint, c_void};

pub type GLenum = c_uint;
pub type GLboolean = c_uchar;
pub type GLbitfield = c_uint;
pub type GLint = c_int;
pub type GLuint = c_uint;
pub type GLsizei = c_int;
pub type GLfloat = c_float;
pub type GLchar = c_char;
pub type GLsizeiptr = isize;

pub const GL_FALSE: GLboolean = 0;
pub const GL_NO_ERROR: GLenum = 0;

pub const GL_TRIANGLES: GLenum = 0x0004;
pub const GL_DEPTH_TEST: GLenum = 0x0B71;
pub const GL_BLEND: GLenum = 0x0BE2;
pub const GL_SCISSOR_TEST: GLenum = 0x0C11;
pub const GL_ONE: GLenum = 1;
pub const GL_ONE_MINUS_SRC_ALPHA: GLenum = 0x0303;
pub const GL_COLOR_BUFFER_BIT: GLbitfield = 0x0000_4000;
pub const GL_FLOAT: GLenum = 0x1406;

pub const GL_VENDOR: GLenum = 0x1F00;
pub const GL_RENDERER: GLenum = 0x1F01;
pub const GL_VERSION: GLenum = 0x1F02;

pub const GL_TEXTURE_2D: GLenum = 0x0DE1;
pub const GL_TEXTURE0: GLenum = 0x84C0;
pub const GL_TEXTURE_MAG_FILTER: GLenum = 0x2800;
pub const GL_TEXTURE_MIN_FILTER: GLenum = 0x2801;
pub const GL_TEXTURE_WRAP_S: GLenum = 0x2802;
pub const GL_TEXTURE_WRAP_T: GLenum = 0x2803;
pub const GL_NEAREST: GLint = 0x2600;
pub const GL_LINEAR: GLint = 0x2601;
pub const GL_CLAMP_TO_EDGE: GLint = 0x812F;
pub const GL_UNPACK_ALIGNMENT: GLenum = 0x0CF5;
pub const GL_UNSIGNED_BYTE: GLenum = 0x1401;
pub const GL_RGBA: GLenum = 0x1908;
pub const GL_MAX_TEXTURE_SIZE: GLenum = 0x0D33;

pub const GL_ARRAY_BUFFER: GLenum = 0x8892;
pub const GL_STATIC_DRAW: GLenum = 0x88E4;

pub const GL_FRAGMENT_SHADER: GLenum = 0x8B30;
pub const GL_VERTEX_SHADER: GLenum = 0x8B31;
pub const GL_COMPILE_STATUS: GLenum = 0x8B81;
pub const GL_LINK_STATUS: GLenum = 0x8B82;
pub const GL_INFO_LOG_LENGTH: GLenum = 0x8B84;

/// Declares the `Gl` struct (one field per entry point) plus its loader, so a
/// new GL call is one line here and nothing else.
///
/// Every field is an `unsafe extern "C"` pointer resolved through
/// `glXGetProcAddressARB` (or `dlsym` for core symbols). The one
/// `unsafe { ... }` the macro emits per entry point is not 49 separate
/// contracts: each one is `Lib::cast_fn`'s, and that function's `# Safety`
/// section is the whole story — a pointer to a function of this exact
/// signature, from this mapping, with the driver's context current on the
/// calling thread.
///
/// What the macro deliberately does *not* do is check that a context is current
/// before an entry point is called. It cannot: `load` runs before any context
/// exists, and the calls happen in `renderer.rs`, not here. The affinity
/// invariant is `Renderer`'s, and it is enforced by the type system there
/// (`Renderer` holds the context as a raw pointer, so it is neither `Send` nor
/// `Sync`).
macro_rules! gl_api {
    ( $( fn $name:ident ( $($arg:ident : $argty:ty),* $(,)? ) $(-> $ret:ty)? ; )+ ) => {
        #[allow(non_snake_case)]
        pub struct Gl {
            $( pub $name: unsafe extern "C" fn($($argty),*) $(-> $ret)?, )+
        }

        impl Gl {
            pub fn load(lib: &Lib) -> Result<Self, String> {
                Ok(Self {
                    // SAFETY: `sym` answered a symbol resolved from `lib`'s own
                    // mapping for exactly `name`, and `cast_fn`'s contract for
                    // that is a pointer to a function of the signature written
                    // on the line below — which is the field's own type, so the
                    // two cannot drift apart.
                    $( $name: unsafe {
                        Lib::cast_fn(lib.sym(stringify!($name))?)
                    }, )+
                })
            }
        }
    };
}

gl_api! {
    fn glGetError() -> GLenum;
    fn glGetString(name: GLenum) -> *const c_uchar;
    fn glViewport(x: GLint, y: GLint, w: GLsizei, h: GLsizei);
    fn glClearColor(r: GLfloat, g: GLfloat, b: GLfloat, a: GLfloat);
    fn glClear(mask: GLbitfield);
    fn glEnable(cap: GLenum);
    fn glDisable(cap: GLenum);
    fn glBlendFunc(src: GLenum, dst: GLenum);
    fn glScissor(x: GLint, y: GLint, w: GLsizei, h: GLsizei);
    fn glFinish();
    fn glFlush();

    fn glCreateShader(kind: GLenum) -> GLuint;
    fn glShaderSource(sh: GLuint, count: GLsizei, src: *const *const GLchar, len: *const GLint);
    fn glCompileShader(sh: GLuint);
    fn glGetShaderiv(sh: GLuint, pname: GLenum, out: *mut GLint);
    fn glGetShaderInfoLog(sh: GLuint, cap: GLsizei, len: *mut GLsizei, log: *mut GLchar);
    fn glDeleteShader(sh: GLuint);

    fn glCreateProgram() -> GLuint;
    fn glAttachShader(prog: GLuint, sh: GLuint);
    fn glLinkProgram(prog: GLuint);
    fn glGetProgramiv(prog: GLuint, pname: GLenum, out: *mut GLint);
    fn glGetProgramInfoLog(prog: GLuint, cap: GLsizei, len: *mut GLsizei, log: *mut GLchar);
    fn glUseProgram(prog: GLuint);
    fn glDeleteProgram(prog: GLuint);
    fn glGetUniformLocation(prog: GLuint, name: *const GLchar) -> GLint;

    fn glUniform1i(loc: GLint, v0: GLint);
    fn glUniform1f(loc: GLint, v0: GLfloat);
    fn glUniform2f(loc: GLint, v0: GLfloat, v1: GLfloat);
    fn glUniform4f(loc: GLint, v0: GLfloat, v1: GLfloat, v2: GLfloat, v3: GLfloat);

    fn glGenBuffers(n: GLsizei, out: *mut GLuint);
    fn glBindBuffer(target: GLenum, buf: GLuint);
    fn glBufferData(target: GLenum, size: GLsizeiptr, data: *const c_void, usage: GLenum);
    fn glDeleteBuffers(n: GLsizei, bufs: *const GLuint);

    fn glGenVertexArrays(n: GLsizei, out: *mut GLuint);
    fn glBindVertexArray(vao: GLuint);
    fn glDeleteVertexArrays(n: GLsizei, vaos: *const GLuint);
    fn glEnableVertexAttribArray(index: GLuint);
    fn glVertexAttribPointer(
        index: GLuint,
        size: GLint,
        kind: GLenum,
        normalized: GLboolean,
        stride: GLsizei,
        offset: *const c_void,
    );
    fn glDrawArrays(mode: GLenum, first: GLint, count: GLsizei);

    fn glGenTextures(n: GLsizei, out: *mut GLuint);
    fn glBindTexture(target: GLenum, tex: GLuint);
    fn glGetTexImage(
        target: GLenum,
        level: GLint,
        format: GLenum,
        kind: GLenum,
        pixels: *mut c_void,
    );
    fn glDeleteTextures(n: GLsizei, texs: *const GLuint);
    fn glTexParameteri(target: GLenum, pname: GLenum, param: GLint);
    fn glActiveTexture(unit: GLenum);
    fn glPixelStorei(pname: GLenum, param: GLint);
    fn glTexImage2D(
        target: GLenum,
        level: GLint,
        internalformat: GLint,
        width: GLsizei,
        height: GLsizei,
        border: GLint,
        format: GLenum,
        ty: GLenum,
        pixels: *const c_void,
    );
    fn glTexSubImage2D(
        target: GLenum,
        level: GLint,
        xoffset: GLint,
        yoffset: GLint,
        width: GLsizei,
        height: GLsizei,
        format: GLenum,
        ty: GLenum,
        pixels: *const c_void,
    );
    fn glGetIntegerv(pname: GLenum, data: *mut GLint);
}

impl Gl {
    /// `glGetString` as a Rust `String` (empty when the driver returns NULL).
    ///
    /// Every caller passes a core `GL_VENDOR`/`GL_RENDERER`/`GL_VERSION`, so
    /// the "unrecognised enum" case — where the driver sets `GL_INVALID_ENUM`
    /// and returns NULL — is handled by the null branch rather than
    /// contemplated.
    pub fn get_string(&self, name: GLenum) -> String {
        // SAFETY: `self.glGetString` is a core-profile 3.3 entry point resolved
        // from the mapping that is still open, and a `glGet*` query needs no
        // current context. The pointer it returns is the driver's own storage
        // for that name, valid until the next `glGetString` with the same
        // `name` or until the context is destroyed — and the string is copied
        // out on the very next line, with no GL call in between.
        let p = unsafe { (self.glGetString)(name) };
        if p.is_null() {
            return String::new();
        }
        // SAFETY: a non-null `glGetString` result is a NUL-terminated string
        // (the GL spec guarantees the terminator is part of the returned
        // bytes), and it is still mapped here because the copy below is the
        // first thing that happens after the call. `CStr::from_ptr` scans only
        // to that terminator, so nothing past the string is read.
        unsafe { std::ffi::CStr::from_ptr(p.cast::<c_char>()) }
            .to_string_lossy()
            .into_owned()
    }

    /// Return the next pending GL error flag, clearing it, or `GL_NO_ERROR` when
    /// the queue is empty. Used after initialization, TFP binds, and frame
    /// submission so a failed draw cannot stay hidden.
    pub fn take_error(&self) -> GLenum {
        // SAFETY: same preconditions as `get_string` — a core entry point from
        // a mapping that is never closed, and a query that needs no context.
        unsafe { (self.glGetError)() }
    }
}
