// maverick/src/core/effect.rs
//
// `Effect` is the vocabulary the *core* uses to tell the *backend* what must
// happen in the outside world as a consequence of a domain decision. It is
// deliberately SEMANTIC, not a set of X11 primitives:
//
//   Engine::dispatch(action) -> mutates State -> returns Vec<Effect>
//   Backend::execute(effect) -> decides HOW (the X11 calls)
//
// The core decides *what* should happen; the backend decides *how*. That is the
// whole point of the split — a future Wayland backend implements the same
// `execute` against the same effects without the core changing.
//
// Note the granularity: `FocusWindow(id)` is a single coarse effect even though
// the X11 backend expands it into ~8 calls (input focus, WM_TAKE_FOCUS, border
// colour, button grabs, _NET_ACTIVE_WINDOW, pointer warp). Those are the "how"
// and stay entirely inside the backend.

use crate::types::{Rect, WindowId};

#[derive(Debug, Clone)]
pub enum Effect {
    /// Recompute + apply the layout geometry for one monitor.
    ArrangeMonitor(usize),
    /// Arrange every monitor (e.g. after a global change).
    ArrangeAll,
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
    MapWindow(WindowId),
    UnmapWindow(WindowId),
    /// Set the fullscreen presentation state for a window, then re-present.
    SetFullscreen { win: WindowId, on: bool },
    /// Recompute a monitor's reserved space / workarea (bar shown/hidden, docks).
    RecalcWorkarea(usize),
    /// Repaint the internal bar for a monitor.
    UpdateBar(usize),
    /// Sync the internal bar window's mapped state to the monitor's `show_bar`.
    SyncBarVisibility(usize),
    /// Refresh EWMH desktop properties (_`NET_NUMBER_OF_DESKTOPS`, current, etc.).
    UpdateEwmhDesktops,
    /// Refresh _`NET_CLIENT_LIST`.
    UpdateClientList,
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
}
