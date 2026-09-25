//! Neutral renderer shim: the single seam between `core`/`backend::x11` and the
//! GPU crates.
//!
//! Everything below is a re-export of `maverick-render`, so no other module
//! names `maverick-gl`/`maverick-vk` directly and the whole GPU backend can be
//! swapped without touching a call site. Keep it a pure re-export — no helper
//! types, no defaults, no state. Context creation, presentation and frame
//! pacing belong to `backend::x11::compositor` and the concrete `Renderer`
//! implementation; the core only ever holds opaque `TextureHandle` values and
//! `Rect`/`DrawQuad` descriptors.

#[allow(unused_imports)]
pub use maverick_render::{
    Acceleration, DrawQuad, Filter, Rect, Renderer, RendererInfo, Texture, TextureHandle,
    VisualDesc,
};
