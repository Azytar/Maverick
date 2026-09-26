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
//!
//! # Status
//!
//! The seam is declared but not yet spanned: `backend::x11::compositor_gl`
//! implements rendering directly against `maverick_gl::Renderer` and does not
//! implement this crate's `Renderer` trait, so nothing in the tree implements
//! `Renderer` or `Texture` and no production code imports the types below.
//! They are re-exported here so the boundary stays in one place and the first
//! backend to target it has a single import root.
//!
//! This is deliberate and not an oversight. See `maverick-render`'s crate docs
//! for the contract and `maverick-vk` for the same pattern applied to an
//! unwired backend.

#[allow(unused_imports)]
pub use maverick_render::{
    Acceleration, DrawQuad, Filter, Rect, Renderer, RendererInfo, Texture, TextureHandle,
    VisualDesc,
};
