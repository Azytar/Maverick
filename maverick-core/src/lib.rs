//! Core domain model — authoritative logical state for placement, focus,
//! workspace membership, layout geometry, floats, cameras, and dock
//! reservations.
//!
//! Pure types: 0 deps beyond `std`, 0 `unsafe`. No X11, GL, or rendering
//! types. The core speaks only `WindowId` (plain `u32`) and `Rect`
//! coordinates, so transitions are testable and reasoned about without an X
//! server or GPU.
//!
//! # `WindowId` abstraction
//!
//! `WindowId = u32` is not an alias for `x11rb::Window`. It is a backend-
//! agnostic handle. The X11 backend converts losslessly at its edges
//! (`as Window` / `as WindowId`); a future Wayland backend would map its own
//! surface handles onto the same id space. XIDs are never reused within a
//! session, so the id is stable for the lifetime of a managed window and safe
//! to use as a `State::clients` key and for focus stacks and deferred-focus
//! slots without stale-reference risk.
//!
//! # Ownership
//!
//! - **Core (this crate)** owns the authoritative logical state: which windows
//!   are tiled/floating, on which monitor/workspace, their focus order, column
//!   weights, camera position/target, and policy (`FullscreenPolicy`, maximize,
//!   focus deferral). What it does not own: X connections, GL contexts,
//!   pixmaps, or rendered frames.
//! - **Backend** owns X11 protocol interaction: reads X11 events, translates
//!   them into `Command`s, applies geometry via `ConfigureWindow`, and mirrors
//!   X11 state back into the core. It must never mutate `State` directly — all
//!   mutations flow through `Command::execute`.
//! - **Compositor** owns visual presentation: it reads `State` and `Cfg`,
//!   computes live (animated) geometry, and draws frames. It does not own
//!   logical state.
//!
//! # Invariants
//!
//! Validated by `State::check_invariants()` in debug after every
//! `Engine::execute()`:
//!
//! A. Every window in exactly one place: either a column's window list
//!    (`Workspace::columns`) or the float list (`Workspace::floats`), never
//!    both and never duplicated across monitors/workspaces.
//! B. `Monitor::active_ws`, `Client::monitor`, and `Client::workspace` agree:
//!    a client's `(monitor, workspace)` matches the workspace it is placed in.
//! C. The scroll camera (`Workspace::camera`) is never the source of truth for
//!    geometry — `arrange_columns` derives positions from `camera.target` for
//!    settled geometry and `camera.position` for live rendering. No drift is
//!    possible; `target`/`position`/`velocity` are finite.
//! D. `Workspace::focused` (`Focus::column_idx`) indexes into `columns`, never
//!    into `floats`; the focused window is always in the column tree or float
//!    list when present, and column `focused` indexes are in range.
//! E. `Monitor::focus_stack` contains only known clients with no duplicates;
//!    `pending_focus` (if set) names live `window` and `owner` and the owner
//!    is still a presented overlay on its `monitor`/`workspace`.
//! F. `Column::weight` is finite and in `[0.05, 1.0]`; `presented_maximize`
//!    (if set) names a maximized client on the active workspace; reserved
//!    regions collapse deterministically into `workarea`.

#![warn(clippy::pedantic)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_lossless,
    clippy::module_name_repetitions,
    clippy::wildcard_imports,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::unreadable_literal,
    clippy::manual_let_else,
    clippy::semicolon_if_nothing_returned,
    clippy::items_after_statements,
    clippy::unused_self,
    clippy::should_implement_trait,
    clippy::struct_excessive_bools,
    clippy::return_self_not_must_use,
    clippy::many_single_char_names
)]

pub mod types;
pub mod wallpaper;

pub use types::{
    Action, Camera, Client, Column, Dir, Edge, Focus, FullscreenPolicy, FullscreenSnapshot,
    LayoutKind, Monitor, PendingFocus, Rect, ReservedArea, ReservedRegion, SizeHints, State,
    ViewportMode, WallpaperCmd, WinFlags, WindowId, WindowMode,
};
