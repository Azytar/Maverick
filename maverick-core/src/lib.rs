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
//!    list (`Workspace::columns`) or a View's float list
//!    (`Workspace::floats`) — and never from two monitors or Views.
//! B. `Client::monitor` and `Client::workspace` agree with the placement tree:
//!    a client's `(monitor, ViewId)` names the View that actually references it.
//!    `ViewId`s are never reused, so a dangling reference is detectable.
//! B2. `Carousel::current` and `Carousel::origin` both name an existing View
//!    whenever the monitor has at least one View (and are both `None` only when
//!    it has none). View identity is independent of carousel position and of any
//!    X11 window id, and navigation is independent of `LayoutKind`.
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

pub mod types;

pub use types::{
    Action, Camera, Carousel, Client, Column, Dir, Edge, Focus, FullscreenPolicy,
    FullscreenSnapshot, LayoutKind, Monitor, PendingFocus, Rect, ReservedArea, ReservedRegion,
    SizeHints, State, ViewId, ViewportMode, WinFlags, WindowId, WindowMode,
};
