// maverick-render/src/lib.rs
//
// Neutral renderer contract for Maverick's compositor layer. Backend-specific
// crates (`maverick-gl`, a future Vulkan backend, ...) implement this trait so
// the WM core never needs to know which renderer is active.

// Public API for downstream backends. The types are constructed by
// `maverick-gl`/`maverick-vk`, not inside this crate, which is the normal
// state for a library crate's stable interface.
#![allow(dead_code)]

use std::fmt;

/// Screen-space rectangle in pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// Quad to draw this frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct DrawQuad {
    pub dst: [f32; 4],
    pub src: [f32; 4],
    pub size: [f32; 2],
    pub radius: f32,
    pub opacity: f32,
}

/// Filter mode for textured quads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    Nearest,
    Linear,
}

/// Opaque handle to a GPU texture. The underlying value is backend-private.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextureHandle(pub u32);

/// Minimal visual description needed to create a texture from a pixmap.
#[derive(Debug, Clone, Copy, Default)]
pub struct VisualDesc {
    pub id: u32,
    pub depth: u8,
    pub direct: bool,
}

/// Hardware vs software acceleration classification.
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

/// Compositor renderer backend.
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
pub trait Texture: Send {
    fn handle(&self) -> TextureHandle;
    fn is_bound(&self) -> bool;
}
