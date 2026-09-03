// maverick/src/backend/renderer.rs
//
// Neutral renderer contract for Maverick's compositor layer.
// Re-exported from `maverick-render` so the WM core never imports
// `maverick-gl`/`maverick-vk` symbols directly.

// Public re-export of the neutral trait. The trait and its types are the
// stable interface for `maverick-gl`/`maverick-vk` backends.
#[allow(unused_imports)]
pub use maverick_render::{
    Acceleration, DrawQuad, Filter, Rect, Renderer, RendererInfo, Texture, TextureHandle,
    VisualDesc,
};
