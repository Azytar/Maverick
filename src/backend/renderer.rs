//! Neutral renderer shim — decouples core/backend from GPU implementations.
//!
//! Role: re-exports the `Renderer` trait and its associated types (`DrawQuad`,
//! `Rect`, `Filter`, `Texture`, `TextureHandle`, `VisualDesc`, `RendererInfo`,
//! `Acceleration`) from `maverick-render` so `core` and `backend::x11` never
//! import `maverick-gl`/`maverick-vk` symbols directly. Swapping the compositor
//! backend requires no changes outside the concrete renderer crate.
//!
//! Boundary: defines no logic, owns no GPU resources, no X types, and no
//! `State`/`Cfg` handles. Presentation, context creation, and frame scheduling
//! are owned by `backend::x11::compositor` and the concrete `Renderer` impl.
//!
//! # Ownership
//!
//! Stateless shim — only `pub use` re-exports. The concrete `Renderer` is
//! created and owned by the X11 backend's compositor; the core holds only
//! opaque `TextureHandle` values and `Rect`/`DrawQuad` descriptors.

#[allow(unused_imports)]
pub use maverick_render::{
    Acceleration, DrawQuad, Filter, Rect, Renderer, RendererInfo, Texture, TextureHandle,
    VisualDesc,
};
