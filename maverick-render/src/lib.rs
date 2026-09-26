//! Neutral renderer contract — backend-agnostic trait boundary for the compositor.
//!
//! This crate holds the `Renderer` trait and the value types that cross the
//! backend boundary (`Rect`, `DrawQuad`, `Filter`, `TextureHandle`,
//! `VisualDesc`, `RendererInfo`). Depending on the trait rather than on a
//! concrete backend is what keeps the rendering implementation swappable
//! without touching window-management logic. No X11 types, no
//! `maverick-core` state, no frame scheduling and no layout geometry belong
//! here.
//!
//! # Ownership
//!
//! A backend owns the GPU context and every resource behind a
//! [`TextureHandle`]; the caller owns the `Renderer`. Textures are created and
//! destroyed through trait methods, and [`Renderer::destroy`] invalidates every
//! handle the renderer handed out. `Renderer: Send` so a caller may move a
//! renderer across threads.
//!
//! Not owned here:
//! - Logical state (`State`, `Client`, `Workspace`, `Monitor`) — `maverick-core`.
//! - X connection, workarea, `ConfigureWindow` placement — the X11 backend.
//! - Frame scheduling, damage tracking, presentation — the compositor.
//! - Window geometry and focus — computed upstream, passed in as `Rect`/
//!   `DrawQuad`.
//!
//! # Frame lifetime
//!
//! `begin_frame` → `draw*` → `end_frame`. [`Renderer::end_frame`] is the only
//! vsync synchroniser; all drawing belongs between `begin_frame` and
//! `end_frame`, and textures are uploaded before the frame that draws them.
//!
//! # Errors
//!
//! Fallible methods return `Err(String)` for logging. A failure means the
//! compositor falls back to the non-composited `ConfigureWindow` path, never
//! that the window manager aborts.
//!
//! # Status
//!
//! This crate is a declared boundary, not a spanned one. The crate has no
//! dependencies — that isolation is the point of it — and it carries its own
//! contract test over the value types. No backend implements [`Renderer`] or
//! [`Texture`] yet: `maverick_gl::Renderer` is a concrete struct that the
//! compositor drives directly, and it does not implement these traits. Nothing
//! in the workspace therefore depends on this crate except the re-export in
//! `src/backend/renderer.rs`, which exists so the seam has one import root
//! when the first backend targets it.
//!
//! It is kept for the same reason `maverick-vk` is kept: an unwired backend
//! boundary that is exercised by its own tests is a deliberate piece of the
//! architecture, whereas a module that nothing declares and nothing references
//! is residue.

#![allow(dead_code)]

use std::fmt;

/// Screen-space rectangle in pixels, top-left origin.
///
/// Carries window geometry, scissor regions and output dimensions; `w`/`h` are
/// unsigned, so a rectangle is never negative.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// Quad to draw this frame.
///
/// `dst` is the destination rectangle in framebuffer pixels
/// (`[x0, y0, x1, y1]`). `src` is the source UV rectangle
/// (`[u0, v0, u1, v1]`, top-down origin). `size` is the texture
/// dimensions used for aspect-ratio correction. `radius` applies
/// rounded corners via the SDF edge function. `opacity` multiplies
/// the sample color (0.0 transparent → 1.0 opaque).
#[derive(Debug, Clone, Copy, Default)]
pub struct DrawQuad {
    pub dst: [f32; 4],
    pub src: [f32; 4],
    pub size: [f32; 2],
    pub radius: f32,
    pub opacity: f32,
}

/// Filter mode for textured quads.
///
/// `Nearest` — pixel-perfect (crisp borders, no bleeding).
/// `Linear` — bilinear sampling (smooth scaling, blur at high zoom).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    Nearest,
    Linear,
}

/// Opaque handle to a backend-owned GPU texture. The wrapped value is
/// backend-private (a `GLuint` in GL); never construct one by hand — obtain it
/// from [`Renderer::upload_rgba`] or [`Renderer::texture_from_pixmap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextureHandle(pub u32);

/// Minimal visual description needed to create a texture from an
/// X pixmap. `id` is the X visual ID; `depth` is the buffer depth
/// in bits; `direct` is true when the visual has direct color
/// rendering (RGB masks, no palette).
#[derive(Debug, Clone, Copy, Default)]
pub struct VisualDesc {
    pub id: u32,
    pub depth: u8,
    pub direct: bool,
}

/// Hardware vs software acceleration classification, reported through
/// [`RendererInfo::accelerated`] so the compositor can log it and, if it needs
/// to, fall back to a software path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Acceleration {
    #[default]
    Unknown,
    Gpu,
    Software,
}

impl fmt::Display for Acceleration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gpu => f.write_str("GPU"),
            Self::Software => f.write_str("Software"),
            Self::Unknown => f.write_str("Unknown"),
        }
    }
}

/// Structured renderer info returned to the compositor for startup logging.
/// `backend` names the backend ("OpenGL/GLX", ...); `vendor`/`renderer`/
/// `version` are the GPU driver strings; `accelerated` reports whether the
/// renderer is on real GPU or a software fallback.
#[derive(Debug, Clone)]
pub struct RendererInfo {
    pub backend: &'static str,
    pub vendor: String,
    pub renderer: String,
    pub version: String,
    pub accelerated: Acceleration,
}

impl fmt::Display for RendererInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Compositor:")?;
        writeln!(f, "  Backend: {}", self.backend)?;
        writeln!(f, "  Vendor: {}", self.vendor)?;
        writeln!(f, "  Renderer: {}", self.renderer)?;
        writeln!(f, "  Version: {}", self.version)?;
        writeln!(f, "  Acceleration: {}", self.accelerated)
    }
}

/// Compositor renderer backend: the entire GPU surface the compositor drives.
///
/// # Ownership
///
/// The caller owns the `Renderer` and must call [`Renderer::destroy`] before
/// dropping it; `destroy` releases every GPU resource and invalidates every
/// texture handle the renderer returned.
///
/// # Thread safety
///
/// `Renderer: Send` so a caller may move the renderer across threads; the trait
/// requires nothing of the context beyond that.
///
/// # Frame lifetime
///
/// ```text
/// begin_frame → draw* → end_frame
/// ```
///
/// # Scissor
///
/// [`Renderer::set_scissor`] takes top-left screen coordinates plus the
/// framebuffer `height`, because a backend whose scissor origin is bottom-left
/// (GL) has to flip `y` against it.
///
/// # Errors
///
/// Fallible methods return `Err(String)`; the caller logs the error and falls
/// back to the non-composited `ConfigureWindow` path.
pub trait Renderer: Send {
    /// Start a frame. `full_clear` controls whether the screen is cleared to
    /// transparent black and scissor disabled before drawing.
    fn begin_frame(&mut self, width: u32, height: u32, full_clear: bool);

    /// Set a scissor rectangle for partial redraw, in top-left screen
    /// coordinates. `height` is the framebuffer height, needed by a backend
    /// that flips `y` into a bottom-left scissor origin.
    fn set_scissor(&mut self, x: i32, y: i32, w: u32, h: u32, height: u32);

    /// Clear the current scissor rectangle.
    fn clear_scissor(&mut self);

    /// Draw one textured quad. The backend binds `tex` itself, so the caller
    /// only has to keep the texture alive for the duration of the call.
    fn draw(&mut self, tex: &mut dyn Texture, quad: &DrawQuad);

    /// Draw a shader-based wallpaper quad. Backends without a shader pipeline
    /// ignore the call — the signature has no way to report that.
    fn draw_shader(&mut self, shader: u32, out: Rect, time: f32, dt: f32);

    /// Finish the frame and present it. The only vsync synchroniser: it blocks
    /// until the frame has reached the screen.
    fn end_frame(&mut self);

    /// Identity of the drawable the compositor renders into, for diagnostics.
    fn debug_drawable(&self) -> u64;

    /// Whether the renderer can read back buffer age for partial redraw.
    fn has_buffer_age(&self) -> bool;

    /// How many frames the back buffer's contents are stale
    /// (`GLX_EXT_buffer_age` semantics), read after a frame has been
    /// presented. `0` means the contents are undefined or the query is
    /// unavailable, and the caller must repaint the whole surface.
    fn back_buffer_age(&self) -> u32;

    /// Wrap an X pixmap as a GPU texture. `visual` must describe the visual
    /// the pixmap was created with; a mismatch shows up as wrong colours.
    fn texture_from_pixmap(
        &mut self,
        pixmap: u32,
        visual: VisualDesc,
        width: u16,
        height: u16,
    ) -> Result<TextureHandle, String>;

    /// Upload `width * height * 4` bytes of RGBA8 (row-major, top-left
    /// origin) as a new texture.
    fn upload_rgba(&mut self, img: &[u8], width: u32, height: u32)
        -> Result<TextureHandle, String>;

    /// Compile a wallpaper fragment shader. Returns an opaque backend shader id.
    fn compile_fragment(&mut self, frag: &str) -> Result<u32, String>;

    /// Destroy a texture previously returned by `upload_rgba`/`texture_from_pixmap`.
    fn destroy_texture(&mut self, handle: TextureHandle);

    /// Release every GPU resource this renderer created and unbind its
    /// context. Invalidates all handles it returned.
    fn destroy(&mut self);

    /// Startup info for logging.
    fn info(&self) -> &RendererInfo;
}

/// Texture object the renderer can draw. Backend-private.
///
/// [`handle`](Texture::handle) must stay stable for the texture's lifetime —
/// the compositor keys its texture bookkeeping on it — and
/// [`is_bound`](Texture::is_bound) must report the backend's real binding
/// state, since a stale `true` lets a backend skip a bind it still needs.
pub trait Texture: Send {
    fn handle(&self) -> TextureHandle;
    fn is_bound(&self) -> bool;
}
