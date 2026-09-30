//! Engine and state-machine side of Maverick.
//!
//! This module owns the *logical* window-manager state machine:
//! what windows exist, where they should be placed, what effects
//! must be applied to X11, and what domain events to publish.
//! It deliberately knows nothing about X11, GL, or rendering —
//! those live in `crate::backend` and `maverick-gl`.
//!
//! # State flow
//!
//! ```text
//! input/event → Engine::dispatch(Action) → Command::execute → effects + Event
//!     → Backend::execute → DesiredState → Reconciler → AppliedState → X11
//! ```
//!
//! - **Command** — a pure mutation of `State` + `Cfg`. The core decides
//!   *what* should happen; commands never touch X11 or GPU handles.
//! - **Effect** — the semantic vocabulary the core uses to tell the
//!   backend what to do. The backend decides *how* (which X11 calls).
//! - **Event** — a domain fact published to subscribers after a command
//!   mutates state. Consumers react without knowing which command caused it.
//! - **`DesiredState`** — the explicit, pure snapshot of where every window
//!   should be, produced by `layout::arrange` + `present::present_into`.
//! - **`AppliedState`** — what X11 *actually* has; the reconciler diffs
//!   Desired vs Applied to emit only the `ConfigureWindow` calls that
//!   are needed.
//!
//! # Modules
//!
//! - `action` — single source of truth for the action vocabulary
//!   (keymap + IPC), preventing drift between TOML and wire format.
//! - `capability` — read-only public API for bars/hooks.
//! - `commands` — typed `Command` implementations for every action.
//! - `desired` — `DesiredState` / `DesiredWindow` for the reconcile pipeline.
//! - `effect` — the `Effect` enum vocabulary.
//! - `engine` — `Engine` struct: central mutation entry point.
//! - `event` — `EventBus` publish/subscribe + domain event types.
//! - `ipc` — JSON serialization for the control socket.
//! - `layout` — columnar layout engine (coordinates only, never stored).
//! - `present` — presentation overlay: fullscreen/maximize geometry rewrites.
//! - `wallpaper` — re-exports + GPU abstraction for the compositor.
//! - `invariants` — test-only end-state contract between the focus pipeline
//!   and the pointer.
//! - `framebench` — test-only heap-allocation counter (allocation-free proof).
//!
//! Graphical sessions — the X server, the window manager and the applications
//! launched into them — are not modelled here. They live in
//! `maverickctl::session` and are driven by `maverickctl session`; this module
//! owns window-management state only.

pub mod action;
pub mod capability;
pub mod commands;
pub mod desired;
pub mod effect;
pub mod engine;
pub mod event;
pub mod ipc;
pub mod layout;
pub mod present;
pub mod wallpaper;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod invariants;

/// Test-only heap-allocation counter used to prove the per-frame compositor
/// path stays allocation-free. Compiled out of the shipped binary.
#[cfg(test)]
pub mod framebench;

#[cfg(test)]
#[global_allocator]
static COUNTING_ALLOCATOR: framebench::Counting = framebench::Counting;

pub use action::{name as action_name, parse as parse_action};
pub use capability::{Query, WindowInfo};
pub use commands::Command;
pub use effect::Effect;
pub use engine::Engine;
pub use event::{CommandReport, Event, EventHandler};
pub use ipc::state_json;
pub use wallpaper::{GpuImage, WallpaperMode, WallpaperSource, WallpaperSpec};
