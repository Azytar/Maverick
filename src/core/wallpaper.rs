//! Wallpaper domain.
//!
//! Re-exports of the pure model from `maverick-core` (`WallpaperMode`,
//! `WallpaperSource`/`WallpaperSpec`, `GpuImage`, `compute_wallpaper_rects`,
//! `shader_is_animated`) plus the decoded RGBA buffer type. Parsing and geometry
//! live in `maverick-core`; decoding lives in `maverick-img`; painting the
//! wallpaper onto the root pixmap is `backend::x11::rootwall`.
//!
//! There is no GPU seam. The wallpaper is drawn by X11 alone, so the core
//! forwards no textures or shader handles to any backend — it holds the
//! wallpaper *description* and nothing else.

/// Re-exported pure helpers/types from `maverick-core`.
pub use maverick_core::wallpaper::{
    compute_wallpaper_rects, shader_is_animated, GpuImage, WallpaperMode, WallpaperSource,
    WallpaperSpec,
};
/// RGBA image buffer produced by `maverick_img::decode`.
pub use maverick_img::Rgba8;
