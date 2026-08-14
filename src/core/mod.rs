pub mod action;
pub mod capability;
pub mod commands;
pub mod desired;
pub mod effect;
pub mod engine;
pub mod event;
pub mod grid;
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
pub use wallpaper::{
    GpuImage, ShaderId, WallpaperGpu, WallpaperMode, WallpaperSource, WallpaperSpec,
};
