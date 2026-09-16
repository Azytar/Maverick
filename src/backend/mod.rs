//! Backend namespace — X11 connection, atom cache, and renderer re-export.
//!
//! Role: groups the platform backends (`x11`), the EWMH/ICCCM atom cache
//! (`atoms`), and the neutral `Renderer` trait shim (`renderer`) under one
//! import root. No logic lives here; each submodule owns its own lifecycle.
//!
//! Boundary: does not own the X connection, the compositor/GL context, or the
//! logical `State` — those are owned by `backend::x11::WindowManager`,
//! `maverick-gl`/`maverick-vk`, and `maverick-core` respectively. This module
//! only declares the submodule tree and re-export surface.
//!
//! # Ownership
//!
//! Stateless. `atoms::Atoms` is created once during `WindowManager::new` and
//! held by the manager; `renderer` re-exports `maverick-render` types without
//! creating them; `x11` owns the event loop and presentation cache.

pub mod atoms;
pub mod renderer;
pub mod x11;
