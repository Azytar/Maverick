//! Backend namespace: the X11 platform backend, the EWMH/ICCCM atom cache and
//! the neutral renderer shim under one import root.
//!
//! Stateless by design — this module only declares the tree. `x11::WindowManager`
//! owns the X connection, the event loop and the `Atoms` it interned at startup;
//! `renderer` re-exports `maverick-render` types without creating them. The
//! logical `State`/`Cfg` live in `maverick-core`.

pub mod atoms;
pub mod renderer;
pub mod x11;
