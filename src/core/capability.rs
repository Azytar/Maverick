//! Capability layer: Maverick's public **read** API.
//!
//! `Query<'a>` borrows `&State` and `WindowInfo` is a stable projection of a
//! managed window. Neither holds a handle, and no method takes `&mut self`:
//! mutation goes exclusively through `Engine::execute(Command)`, so external
//! consumers never depend on the internal `State`/`Monitor`/`Workspace`/
//! `Client` model, and the writer keeps a single entry path.
//!
//! A bar, a hook or an external tool must not walk the internal state — that
//! model may change in any version. It asks this layer stable questions
//! instead:
//!
//! ```ignore
//! let q = engine.query();
//! q.active_workspace();   // → which workspace is visible?
//! q.focused_window();     // → which window has focus?
//! q.visible_windows();    // → which windows are on screen now?
//! q.current_layout();     // → which layout is active?
//! ```
//!
//! Compass rule: a query earns its existence only by serving a bar, a hook
//! and a test at once (three consumers). Nothing speculative is added.

use crate::types::{LayoutKind, State, WindowId};

/// Stable public information about one window. Deliberately decoupled from the
/// internal `Client` so the internal model can evolve without breaking external
/// consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub id: WindowId,
    pub title: String,
    pub class: String,
    pub instance: String,
    pub floating: bool,
    pub fullscreen: bool,
    pub workspace: usize,
    pub monitor: usize,
}

/// Read-only view over the WM state. Borrows `&State` and exposes only stable
/// queries — never mutation.
pub struct Query<'a> {
    state: &'a State,
}

impl<'a> Query<'a> {
    /// Borrow `State` for read-only queries. No mutation path exists on `Query`.
    pub fn new(state: &'a State) -> Self {
        Self { state }
    }

    /// Number of live monitors.
    pub fn monitor_count(&self) -> usize {
        self.state.monitors.len()
    }

    /// Selected monitor index, clamped to `monitor_count - 1`: `sel_mon` can
    /// still name a monitor that a hotplug removed, and every query below
    /// indexes with it.
    pub fn selected_monitor(&self) -> usize {
        self.state
            .sel_mon
            .min(self.monitor_count().saturating_sub(1))
    }

    /// Active workspace index on the selected monitor, `0` if the monitor
    /// vanished between the clamp and the lookup.
    pub fn active_workspace(&self) -> usize {
        self.state
            .monitors
            .get(self.selected_monitor())
            .map_or(0, |m| m.active_ws)
    }

    /// Number of workspaces on the selected monitor.
    pub fn workspace_count(&self) -> usize {
        self.state
            .monitors
            .get(self.selected_monitor())
            .map_or(0, |m| m.workspaces.len())
    }

    /// Layout active on the selected monitor's active workspace.
    pub fn current_layout(&self) -> LayoutKind {
        self.state
            .monitors
            .get(self.selected_monitor())
            .and_then(|m| m.workspaces.get(m.active_ws))
            .map_or(LayoutKind::Column, |w| w.layout)
    }

    /// Focused window on the selected monitor, if any.
    pub fn focused_window(&self) -> Option<WindowId> {
        self.state
            .monitors
            .get(self.selected_monitor())
            .and_then(|m| m.focused)
    }

    /// IDs of every window on the active workspace (tiled + floating), in
    /// column order with the floats last.
    pub fn visible_windows(&self) -> Vec<WindowId> {
        let mi = self.selected_monitor();
        let Some(m) = self.state.monitors.get(mi) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if let Some(w) = m.workspaces.get(m.active_ws) {
            for col in &w.columns {
                out.extend(col.windows.iter().copied());
            }
            out.extend(w.floats.iter().copied());
        }
        out
    }

    /// Public information for one window, if it is still managed.
    pub fn window(&self, id: WindowId) -> Option<WindowInfo> {
        let c = self.state.clients.get(&id)?;
        Some(WindowInfo {
            id: c.window,
            title: c.name.clone(),
            class: c.class.clone(),
            instance: c.instance.clone(),
            floating: c.is_float(),
            fullscreen: c.is_fullscreen(),
            workspace: c.workspace,
            monitor: c.monitor,
        })
    }

    /// Public information for every managed window, in unspecified order.
    pub fn windows(&self) -> Vec<WindowInfo> {
        self.state
            .clients
            .values()
            .filter_map(|c| self.window(c.window))
            .collect()
    }
}
