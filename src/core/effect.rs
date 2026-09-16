//! Effect vocabulary — the semantic contract between core and backend.
//!
//! What owns: `Effect` — the enum the core uses to tell the backend what must
//! happen in the outside world as a consequence of a domain decision. It is
//! deliberately semantic, not a bag of X11 primitives.
//!
//! Exposes: `Effect` variants (`ArrangeMonitor`, `FocusWindow`, `ConfigureWindow`,
//! `SetFullscreen`/`SetMaximized`, `SyncWindowPrefs`, etc.) — the single
//! vocabulary `Engine::dispatch`/`Engine::execute` return and `Backend::execute`
//! consumes.
//!
//! Leaves to others: *how* each effect is carried out (which X11/GL calls,
//! ordering, error handling). The core decides *what*; the backend decides *how*.
//! A future Wayland backend implements the same `execute` against the same
//! effects without core changes.
//!
//! Invariants: coarse granularity is intentional — e.g. `FocusWindow(id)` is one
//! effect even though the X11 backend expands it into ~8 calls (input focus,
//! `WM_TAKE_FOCUS`, border, grabs, `_NET_ACTIVE_WINDOW`, warp). `Effect` never
//! carries X11 handles or `State` refs.
//!
//! Flow: `Engine::dispatch(Action) → mutates State → Vec<Effect> → Backend::execute`.

use crate::types::{Rect, WindowId};

/// Semantic effect vocabulary: what the core asks the backend to do.
///
/// The core decides *what* (which variant, which `WindowId`/`Rect`); the backend
/// decides *how* (which X11/GL calls). Each `Engine::execute` returns a `Vec<Self>`
/// that `Backend::execute` drains.
#[derive(Debug, Clone)]
pub enum Effect {
    /// Recompute + apply the layout geometry for one monitor.
    ArrangeMonitor(usize),
    /// Mark a monitor's stacking order dirty (float/fullscreen changed), so the
    /// next arrange restacks. Emit before `ArrangeMonitor` when z-order changed.
    MarkRestack(usize),
    /// Move focus to a window (or clear it with `None`). The backend performs
    /// all the X11 focus plumbing.
    FocusWindow(Option<WindowId>),
    /// Drop the focus decorations/grabs from a window without focusing another
    /// (used when leaving a monitor before focusing on the new one).
    Unfocus(WindowId),
    /// Place a single window at an absolute rect with the given border width.
    /// (This is the old `MoveResize`; emitted by the layout arrange loop.)
    ConfigureWindow {
        win: WindowId,
        geom: Rect,
        border_w: u32,
    },
    /// Ask the window to close (`WM_DELETE_WINDOW`, else kill).
    KillWindow(WindowId),
    /// Set the fullscreen presentation state for a window, then re-present.
    SetFullscreen { win: WindowId, on: bool },
    /// Set the maximized (workarea-filling) presentation state for a window,
    /// then re-present. Only presented while the window is focused (peek).
    ///
    /// The two EWMH axes (`_NET_WM_STATE_MAXIMIZED_VERT` / `_..._HORZ`) are
    /// independent: `None` means "leave that axis as it is", which is what a
    /// client message naming only one of them asks for.
    SetMaximized {
        win: WindowId,
        vert: Option<bool>,
        horiz: Option<bool>,
    },
    /// Persist the window's private float/geometry atoms (used across WM
    /// restart / `--replace`). A SEMANTIC effect so the backend keeps its
    /// persistence format its own business.
    SyncWindowPrefs(WindowId),
    /// Set _`NET_CURRENT_DESKTOP` on the root window.
    SetCurrentDesktop(usize),
    /// Set _`NET_WM_DESKTOP` on a window.
    SetWindowDesktop { win: WindowId, ws: usize },
    /// Launch an external process.
    Spawn(Vec<String>),
    /// Terminate the WM cleanly.
    Quit,
    /// Re-exec the WM binary in place.
    Restart,
    /// Publish the current state snapshot to IPC subscribers.
    PublishIpcState,
    /// Apply the engine's current `state.wallpaper` to the compositor: decode +
    /// upload (or compile a shader) and request one full repaint. Emitted by
    /// `SetWallpaper`; the backend decides HOW (GL calls stay in x11/GL).
    SetWallpaper,
}
