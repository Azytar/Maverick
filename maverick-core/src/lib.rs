//! Core domain model — authoritative logical state for placement, focus,
//! workspace membership, layout geometry, floats, cameras, and dock
//! reservations.
//!
//! `std`-only, no `unsafe`, and free of X11/GL/rendering types: the core speaks
//! `WindowId` (a plain `u32`) and `Rect`, so every transition can be reasoned
//! about and tested without an X server or a GPU.
//!
//! # Ownership
//!
//! - **Core (this crate)** owns the authoritative logical state: which windows
//!   are tiled/floating, on which monitor/workspace, their focus order, column
//!   weights, camera position/target, and policy (`FullscreenPolicy`, maximize,
//!   focus deferral). Not owned here: X connections, GL contexts, pixmaps, or
//!   rendered frames.
//! - **Backend** owns X11 protocol interaction: it reads X11 events, translates
//!   them into `Command`s, applies geometry via `ConfigureWindow`, and draws
//!   the decorations. It must never mutate `State` directly — all mutations
//!   flow through `Command::execute`.
//!
//! [`WindowId`] is a backend-agnostic handle rather than an alias for
//! `x11rb::Window`; see its documentation for the id-stability invariant that
//! lets it key `State::clients`, focus stacks, and deferred-focus slots.
//!
//! # Invariants
//!
//! Checked by [`State::check_invariants`], which `State::assert_invariants`
//! runs after every `Engine::execute` / `execute_batch` in debug builds:
//!
//! A. Every window is referenced from exactly one place — a column's window
//!    list (`Workspace::columns`) or a workspace's float list
//!    (`Workspace::floats`) — and never from two monitors or workspaces.
//! B. `Monitor::active_ws`, `Client::monitor`, and `Client::workspace` agree:
//!    a client's `(monitor, workspace)` is the workspace it is placed in.
//! C. The scroll camera is an *input* to the projection, not a source of
//!    truth: `arrange_columns` derives each column's x from `camera.position`,
//!    so the layout is a pure function of the state. `position` is checked
//!    for finiteness, and every `Camera` mutator refuses a non-finite value.
//! D. `Workspace::focus` (`Focus::column_idx`) indexes `columns`, never
//!    `floats`, and every `Column::focused` is in range.
//! E. `Monitor::focus_stack` holds only known clients, without duplicates;
//!    `pending_focus`, if set, names a live `window` and `owner`, and that
//!    owner is still a presented overlay on the deferral's own
//!    `monitor`/`workspace`.
//! F. `Column::weight` is finite and within `[0.05, 1.0]`; `presented_maximize`,
//!    if set, names a maximized client on the active workspace. Not
//!    machine-checked but equally binding: `Monitor::workarea` is always
//!    `Monitor::screen` minus the collapsed `reserved` totals.

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

pub use types::{
    Action, Camera, Client, Column, Dir, Edge, Focus, FullscreenPolicy, FullscreenSnapshot,
    LayoutKind, Monitor, PendingFocus, Rect, ReservedArea, ReservedRegion, SizeHints, State,
    ViewportMode, WinFlags, WindowId, WindowMode,
};
