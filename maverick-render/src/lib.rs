//! Neutral renderer contract — backend-agnostic trait boundary for the compositor.
//!
//! This crate defines the `Renderer` trait and the types that flow between the
//! compositor and the GPU backends (`maverick-gl` and a future Vulkan backend).
//! The WM core never imports `maverick-gl` or `maverick-vk` symbols — it only
//! talks to this crate, so the rendering backend can be swapped without touching
//! window-management logic. No X11, no `maverick-core` state, no frame
//! scheduling or layout geometry lives here.
//!
//! # Ownership
//!
//! The `Renderer` trait owns no state beyond the GPU context itself. Textures
//! are created and destroyed by the backend; the compositor only holds opaque
//! `TextureHandle` values. The backend is `Send` so the renderer can move
//! across threads if the architecture requires it.
//!
//! What this crate does **not** own:
//! - Logical state (`State`, `Client`, `Workspace`, `Monitor`) — owned by
//!   `maverick-core`.
//! - X connection, workarea, or `ConfigureWindow` placement — owned by the X11
//!   backend.
//! - Frame scheduling, damage tracking, or presentation logic — owned by the
//!   compositor.
//! - Window geometry or focus — computed upstream and passed in as `Rect`/
//!   `DrawQuad`.
//!
//! # Lifecycle
//!
//! 1. Backend creates a `Renderer` (GL/Vk context current).
//! 2. Compositor calls `begin_frame` → `draw` (repeated) → `end_frame`.
//! 3. Textures are uploaded (`upload_rgba` / `texture_from_pixmap`) and
//!    destroyed (`destroy_texture`) by the compositor when windows unmap or the
//!    compositor shuts down.
//! 4. On shutdown the compositor calls `destroy`, which cleans up all GPU
//!    resources and makes the context current-free.
//!
//! # Errors
//!
//! All fallible operations return `Result<_, String>`. The compositor logs
//! errors and falls back to the non-composited `ConfigureWindow` path rather
//! than crashing.

#![allow(dead_code)]

use std::fmt;

/// Screen-space rectangle in pixels.
///
/// Used for window geometry, scissor regions, and output dimensions.
/// Coordinates are top-left origin; `w`/`h` are always non-negative.
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

/// Opaque handle to a GPU texture. The underlying value is
/// backend-private — `u32` in GL (a `GLuint`), `u32` in Vulkan
/// (a `VkImageView` index). Do not construct manually; always
/// obtain via `upload_rgba` or `texture_from_pixmap`.
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

/// Hardware vs software acceleration classification.
/// Returned by `RendererInfo::acceleration` so the compositor
/// can log and, if needed, fall back to software path.
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
/// `backend` is a static string identifying the crate
/// ("OpenGlGlx" / "Vulkan"); `vendor`/`renderer`/`version`
/// are the GPU driver strings; `accelerated` classifies
/// whether the renderer is using real GPU or software fallback.
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

/// Compositor renderer backend. Implemented by `maverick-gl` and
/// a future Vulkan backend. The WM core calls only this trait,
/// so the rendering implementation can be swapped without
/// touching the window manager.
///
/// # Ownership
///
/// The caller owns the `Renderer` and must call `destroy` before
/// dropping it to release GPU resources. All texture handles
/// created by this renderer are invalidated by `destroy`.
///
/// # Thread safety
///
/// `Renderer: Send` so the compositor can move the renderer
/// across threads if needed. In practice the renderer lives
/// on the main thread.
///
/// # Frame lifetime
///
/// ```text
/// begin_frame → draw* → end_frame
/// ```
/// `end_frame` is the only vsync synchroniser; it blocks until
/// the frame is presented. All drawing must happen between
/// `begin_frame` and `end_frame`.
///
/// # Scissor
///
/// `set_scissor` uses a bottom-left origin for `y` because
/// that is what GL uses. The caller must pass the framebuffer
/// `height` so the scissor can be flipped to GL's coordinate
/// space.
///
/// # Errors
///
/// All fallible operations return `Err(String)`. The compositor
/// logs the error and falls back to the non-composited
/// `ConfigureWindow` path rather than crashing the WM.
pub trait Renderer: Send {
    /// Start a frame. `full_clear` controls whether the screen is cleared to
    /// transparent black and scissor disabled before drawing.
    fn begin_frame(&mut self, width: u32, height: u32, full_clear: bool);

    /// Set a scissor rectangle for partial redraw. `height` is the framebuffer
    /// height because some backends (GL) use a bottom-left scissor origin.
    fn set_scissor(&mut self, x: i32, y: i32, w: u32, h: u32, height: u32);

    /// Clear the current scissor rectangle.
    fn clear_scissor(&mut self);

    /// Draw one textured quad. `tex` is an opaque texture bound for this draw;
    /// the renderer may consume it if needed.
    fn draw(&mut self, tex: &mut dyn Texture, quad: &DrawQuad);

    /// Draw a shader-based wallpaper quad (if this backend supports it).
    fn draw_shader(&mut self, shader: u32, out: Rect, time: f32, dt: f32);

    /// Finish the frame and present. This is the only vsync synchroniser.
    fn end_frame(&mut self);

    /// Identity of the drawable used as the compositor framebuffer.
    fn debug_drawable(&self) -> u64;

    /// Whether the renderer can read back buffer age for partial redraw.
    fn has_buffer_age(&self) -> bool;

    /// Number of back-buffer ages observed. Returns 0 when unavailable.
    fn back_buffer_age(&self) -> u32;

    /// Wrap an X pixmap as a GPU texture.
    fn texture_from_pixmap(
        &mut self,
        pixmap: u32,
        visual: VisualDesc,
        width: u16,
        height: u16,
    ) -> Result<TextureHandle, String>;

    /// Upload CPU RGBA8 pixels as a new texture.
    fn upload_rgba(&mut self, img: &[u8], width: u32, height: u32)
        -> Result<TextureHandle, String>;

    /// Compile a wallpaper fragment shader. Returns an opaque backend shader id.
    fn compile_fragment(&mut self, frag: &str) -> Result<u32, String>;

    /// Destroy a texture previously returned by `upload_rgba`/`texture_from_pixmap`.
    fn destroy_texture(&mut self, handle: TextureHandle);

    /// Destroy all renderer resources and make the context current-free.
    fn destroy(&mut self);

    /// Startup info for logging.
    fn info(&self) -> &RendererInfo;
}

/// Texture object the renderer can draw. Backend-private.
///
/// Implementors must ensure that `handle()` returns the
/// same value for the lifetime of the texture, and that
/// `is_bound()` accurately reflects whether the texture
/// is currently bound to the GL context. A texture that
/// has been `destroy_texture`d must return `false` from
/// `is_bound`.
pub trait Texture: Send {
    fn handle(&self) -> TextureHandle;
    fn is_bound(&self) -> bool;
}
