//! Backend namespace: the X11 platform backend and the EWMH/ICCCM atom cache
//! under one import root.
//!
//! Stateless by design — this module only declares the tree. `x11::WindowManager`
//! owns the X connection, the event loop and the `Atoms` it interned at startup.
//! The logical `State`/`Cfg` live in `maverick-core`.

pub mod atoms;
pub mod x11;
