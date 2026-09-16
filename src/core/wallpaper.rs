// maverick/src/core/wallpaper.rs
//
// The wallpaper *domain model* — kept entirely free of any GL/X11 type so the
// upper layers (State, Engine, WindowManager) never name OpenGL. The actual GPU
// work goes through the `WallpaperGpu` trait (implemented inside the x11/GL
// backend as `GlWallpaper`), which is the seam the plan requires for a future
// Vulkan backend.

pub use maverick_core::wallpaper::WallpaperMode as _WallpaperModeCheck;
/// Re-exported pure helpers/types from `maverick-core`.
pub use maverick_core::wallpaper::{
    compute_wallpaper_rects, shader_is_animated, GpuImage, WallpaperMode, WallpaperSource,
    WallpaperSpec,
};
/// RGBA image buffer shared with `WallpaperGpu::upload_image`.
pub use maverick_img::Rgba8;

/// The GPU abstraction the wallpaper needs. Implemented by the x11/GL backend
/// (`GlWallpaper`); a future Vulkan backend implements the same trait. The core
/// only ever calls these methods — it never speaks OpenGL.
///
/// `ShaderId` is the backend's own opaque program/pipeline handle (GL:
/// `GLuint`). The core treats it as opaque, only forwarding it back to the
/// backend, so it does not define its own copy here.
#[cfg(feature = "compositor-opengl")]
pub trait WallpaperGpu {
    fn upload_image(&mut self, img: &Rgba8) -> Result<GpuImage, String>;
    fn compile_shader(&mut self, frag: &str) -> Result<maverick_gl::ShaderId, String>;
    fn draw_image(&mut self, img: &GpuImage, dst: crate::types::Rect, src_uv: [f32; 4]);
    fn draw_shader(
        &mut self,
        s: maverick_gl::ShaderId,
        out: crate::types::Rect,
        time: f32,
        dt: f32,
    );
    fn release(&mut self, img: GpuImage);
}
