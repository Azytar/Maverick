// maverick/src/types.rs
// Core state — niri-style columnar layout, clean coordinates, no drift.

use std::collections::HashMap;

/// Backend-agnostic window identifier used throughout the core domain model.
///
/// This is a plain `u32`, not an alias for x11rb's `Window` — the core must not
/// import any X11 protocol type. On X11 the backend's `Window` (itself a u32
/// XID) converts losslessly to/from `WindowId` at the backend's edges (`as
/// Window` / `as WindowId`). A future Wayland backend would map its own surface
/// handles onto this same id space instead. The frontier is strict: the core
/// speaks only `WindowId`, the backend does the conversion.
pub type WindowId = u32;

// ─── Geometry ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    #[inline]
    pub fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }
    #[inline]
    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.x + self.w as i32 && py >= self.y && py < self.y + self.h as i32
    }
    #[inline]
    pub fn area(&self) -> u64 {
        self.w as u64 * self.h as u64
    }
    #[inline]
    pub fn right(&self) -> i32 {
        self.x + self.w as i32
    }
    #[inline]
    pub fn bottom(&self) -> i32 {
        self.y + self.h as i32
    }
}

// ─── Window flags ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default)]
pub struct WinFlags(u8);
impl WinFlags {
    pub const FLOAT: u8 = 1 << 0;
    pub const FULLSCREEN: u8 = 1 << 1;
    pub const URGENT: u8 = 1 << 2;
    pub const NO_FOCUS: u8 = 1 << 3;
    pub const FIXED: u8 = 1 << 4;
    pub const MAXIMIZED: u8 = 1 << 5;
    /// Sticky: a float that stays visible on every workspace of its monitor
    /// (never hidden by `hide_offscreen`). Set via a window rule.
    pub const STICKY: u8 = 1 << 6;

    #[inline]
    pub fn set(&mut self, f: u8) {
        self.0 |= f;
    }
    #[inline]
    pub fn clear(&mut self, f: u8) {
        self.0 &= !f;
    }
    #[inline]
    pub fn toggle(&mut self, f: u8) {
        self.0 ^= f;
    }
    #[inline]
    pub fn has(&self, f: u8) -> bool {
        self.0 & f != 0
    }
}

// ─── Size hints ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default)]
pub struct SizeHints {
    pub base_w: i32,
    pub base_h: i32,
    pub inc_w: i32,
    pub inc_h: i32,
    pub max_w: i32,
    pub max_h: i32,
    pub min_w: i32,
    pub min_h: i32,
    pub min_aspect: f32,
    pub max_aspect: f32,
    pub valid: bool,
}

// ─── Column (niri-style) ──────────────────────────────────────────────────────
//
// Each workspace has N columns. Every column holds one or more windows
// stacked vertically. The layout engine assigns absolute screen coords
// based on the column's logical offset and the current scroll position.
//
// This means coordinates are ALWAYS derived from (col_x + scroll, row_y)
// and never stored as mutable state — no drift possible.

#[derive(Debug, Clone)]
pub struct Column {
    pub windows: Vec<WindowId>, // top-to-bottom
    pub width: u32,             // pixel width of this column
    pub focused: usize,         // index into `windows` that has focus
}

impl Column {
    pub fn new(width: u32) -> Self {
        Self {
            windows: Vec::with_capacity(4),
            width,
            focused: 0,
        }
    }
    pub fn focused_win(&self) -> Option<WindowId> {
        self.windows.get(self.focused).copied()
    }
}

// ─── Workspace ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Focus {
    pub column_idx: usize,
}

#[derive(Debug, Clone)]
pub struct Workspace {
    pub tag: u32,
    pub columns: Vec<Column>,
    pub focus: Focus,
    pub scroll: i32,
    pub floats: Vec<WindowId>,
    /// Layout mode for this specific workspace — independent of every other workspace.
    pub layout: LayoutKind,
}

impl Workspace {
    pub fn new(tag: u32) -> Self {
        Self {
            tag,
            columns: Vec::new(),
            focus: Focus { column_idx: 0 },
            scroll: 0,
            floats: Vec::new(),
            layout: LayoutKind::Column,
        }
    }

    pub fn empty(tag: u32) -> Self {
        Self::new(tag)
    }

    pub fn is_empty(&self) -> bool {
        self.columns.is_empty() && self.floats.is_empty()
    }

    pub fn focused_win(&self) -> Option<WindowId> {
        self.columns.get(self.focus.column_idx)?.focused_win()
    }

    /// Advance this workspace's layout Column→Grid→Column.
    /// Pure state mutation (no X11); the single source of truth shared by the
    /// backend's `do_action` and the core `Engine`.
    pub fn cycle_layout(&mut self) -> LayoutKind {
        self.layout = match self.layout {
            LayoutKind::Column => LayoutKind::Grid,
            LayoutKind::Grid => LayoutKind::Column,
        };
        self.layout
    }

    /// P3: &mut self — no clone needed at call sites
    pub fn add_tiled(&mut self, window: WindowId, _default_col_width: u32, workarea_w: u32) {
        if self.columns.is_empty() {
            // First window: occupies the entire workarea (100%)
            let mut col = Column::new(workarea_w);
            col.windows.push(window);
            self.columns.push(col);
            self.focus.column_idx = 0;
        } else {
            // Second window onwards: 75% of the workarea
            let new_col_w = (workarea_w as f32 * 0.75) as u32;
            let mut new_col = Column::new(new_col_w);
            new_col.windows.push(window);
            self.columns.push(new_col);
            self.focus.column_idx = self.columns.len() - 1;
        }
    }

    /// P3: &mut self — no clone needed at call sites
    pub fn remove_window(&mut self, win: WindowId) {
        if let Some(fi) = self.floats.iter().position(|&w| w == win) {
            self.floats.remove(fi);
            return;
        }

        for col in &mut self.columns {
            if let Some(wi) = col.windows.iter().position(|&w| w == win) {
                col.windows.remove(wi);
                if col.focused >= col.windows.len() && !col.windows.is_empty() {
                    col.focused = col.windows.len() - 1;
                }
                break;
            }
        }

        self.cleanup_empty_columns();
    }

    /// P3: &mut self — no clone needed at call sites
    pub fn cleanup_empty_columns(&mut self) {
        self.columns.retain(|col| !col.windows.is_empty());

        if self.columns.is_empty() {
            self.focus.column_idx = 0;
        } else if self.focus.column_idx >= self.columns.len() {
            self.focus.column_idx = self.columns.len() - 1;
        }
    }
}

// ─── Client ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Client {
    pub window: WindowId,
    pub name: String,
    pub class: String,
    pub instance: String,
    pub geom: Rect,
    pub saved_geom: Rect,
    pub border_w: u32,
    pub old_border_w: u32,
    /// Window opacity as 0.0-1.0, from the best matching rule. Written to the
    /// X11 property `_NET_WM_WINDOW_OPACITY` at manage/rearrange time. `None`
    /// means "use the global default" (fully opaque).
    pub opacity: Option<f32>,
    pub flags: WinFlags,
    pub hints: SizeHints,
    pub monitor: usize,
    pub workspace: usize, // index into Monitor::workspaces
    /// The window this one is transient for (`WM_TRANSIENT_FOR`), when it was a
    /// known client at manage time. Used by the renderer to keep popups/dialogs
    /// of a fullscreen or maximized window above the presentation overlay.
    pub transient_parent: Option<WindowId>,
    /// `_NET_WM_WINDOW_TYPE` values this window declared, as lowercase atom
    /// names (`"dialog"`, `"utility"`, `"toolbar"`, …). Used by window rules.
    pub window_types: Vec<String>,
    pub focus_serial: u64,
    pub is_unmanaged: bool,
    pub wants_input: bool,
    pub wm_hidden: bool,
}

impl Client {
    pub fn new(win: WindowId, mon: usize, ws: usize) -> Self {
        Self {
            window: win,
            name: String::new(),
            class: String::new(),
            instance: String::new(),
            geom: Rect::default(),
            saved_geom: Rect::default(),
            border_w: 2,
            old_border_w: 2,
            opacity: None,
            flags: WinFlags::default(),
            hints: SizeHints::default(),
            monitor: mon,
            workspace: ws,
            transient_parent: None,
            window_types: Vec::new(),
            focus_serial: 0,
            is_unmanaged: false,
            wants_input: true,
            wm_hidden: false,
        }
    }

    #[inline]
    pub fn is_float(&self) -> bool {
        self.flags.has(WinFlags::FLOAT)
    }
    #[inline]
    pub fn is_fullscreen(&self) -> bool {
        self.flags.has(WinFlags::FULLSCREEN)
    }
    #[inline]
    pub fn is_maximized(&self) -> bool {
        self.flags.has(WinFlags::MAXIMIZED)
    }
    #[inline]
    pub fn no_focus(&self) -> bool {
        self.flags.has(WinFlags::NO_FOCUS)
    }
    #[inline]
    pub fn is_sticky(&self) -> bool {
        self.flags.has(WinFlags::STICKY)
    }
}

// ─── Reserved space ─────────────────────────────────────────────────────────────
//
// Reservation is modelled in two layers:
//
//   ReservedRegion[]  — individual, trackable reservations (one per dock/bar),
//                       each tagged with its owner so it can be removed exactly
//                       when that window disappears.
//   ReservedArea      — the collapsed per-edge totals, derived from the regions.
//
// The layout only ever sees the resulting `workarea` (screen − ReservedArea) and
// knows nothing about *what* reserved the space. This is deliberately
// backend-agnostic: on X11 the regions come from each external dock's
// `_NET_WM_STRUT[_PARTIAL]`; a future Wayland backend would fill the same
// regions from layer-shell exclusive zones.

/// Which screen edge a reservation pushes in from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

/// A single trackable reservation. `owner` identifies the source: external
/// docks use their window id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservedRegion {
    pub owner: WindowId,
    pub edge: Edge,
    /// Thickness in px pushed in from `edge`.
    pub thickness: u32,
}

/// Collapsed per-edge reservation totals derived from a set of regions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReservedArea {
    pub top: u32,
    pub bottom: u32,
    pub left: u32,
    pub right: u32,
}

impl ReservedArea {
    /// Collapse trackable regions into per-edge totals. Multiple reservations on
    /// the same edge stack (e.g. two docks both reserving the top).
    pub fn from_regions(regions: &[ReservedRegion]) -> Self {
        let mut a = ReservedArea::default();
        for r in regions {
            let slot = match r.edge {
                Edge::Top => &mut a.top,
                Edge::Bottom => &mut a.bottom,
                Edge::Left => &mut a.left,
                Edge::Right => &mut a.right,
            };
            *slot = slot.saturating_add(r.thickness);
        }
        a
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.top == 0 && self.bottom == 0 && self.left == 0 && self.right == 0
    }
}

// ─── Monitor ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Monitor {
    pub screen: Rect,
    pub workarea: Rect, // screen minus reserved (derived)
    /// Individual trackable reservations (one per external dock).
    pub reserved_regions: Vec<ReservedRegion>,
    /// Collapsed per-edge totals, derived from `reserved_regions`.
    pub reserved: ReservedArea,
    pub workspaces: Vec<Workspace>,
    pub active_ws: usize,
    pub focused: Option<WindowId>,
    pub focus_stack: Vec<WindowId>,
}

impl Monitor {
    pub fn new(screen: Rect, n_tags: usize) -> Self {
        let workspaces = (0..n_tags).map(|i| Workspace::new(i as u32)).collect();
        let mut m = Self {
            screen,
            workarea: screen,
            reserved_regions: Vec::new(),
            reserved: ReservedArea::default(),
            workspaces,
            active_ws: 0,
            focused: None,
            focus_stack: Vec::with_capacity(16),
        };
        m.recalc_geometry();
        m
    }

    pub fn ws(&self) -> &Workspace {
        &self.workspaces[self.active_ws]
    }
    pub fn ws_mut(&mut self) -> &mut Workspace {
        &mut self.workspaces[self.active_ws]
    }

    // ── Reservation management ──────────────────────────────────────────────
    //
    // All mutations go through these helpers so `reserved` and `workarea` stay
    // consistent with `reserved_regions` (the single source of truth).

    /// Insert or replace the region owned by `owner`. `thickness == 0` removes it.
    pub fn set_reserved_region(&mut self, owner: WindowId, edge: Edge, thickness: u32) {
        self.reserved_regions.retain(|r| r.owner != owner);
        if thickness > 0 {
            self.reserved_regions.push(ReservedRegion {
                owner,
                edge,
                thickness,
            });
        }
        self.recalc_geometry();
    }

    /// Remove any region owned by `owner`. Returns true if something was removed.
    pub fn remove_reserved_region(&mut self, owner: WindowId) -> bool {
        let before = self.reserved_regions.len();
        self.reserved_regions.retain(|r| r.owner != owner);
        let removed = self.reserved_regions.len() != before;
        if removed {
            self.recalc_geometry();
        }
        removed
    }

    /// Recompute `reserved` and `workarea` from `reserved_regions`.
    pub fn recalc_geometry(&mut self) {
        self.reserved = ReservedArea::from_regions(&self.reserved_regions);
        let r = self.reserved;
        let x = self.screen.x + r.left as i32;
        let y = self.screen.y + r.top as i32;
        let w = self.screen.w.saturating_sub(r.left + r.right);
        let h = self.screen.h.saturating_sub(r.top + r.bottom);
        self.workarea = Rect::new(x, y, w, h);
    }

    /// Grow or shrink the workspace slots to match `n_tags`, preserving window
    /// state for indices that survive. Growing appends fresh empty workspaces;
    /// shrinking drops trailing slots (windows still assigned there are clamped
    /// to the last surviving workspace by the caller). Keeps `active_ws` in range.
    pub fn reconcile_workspaces(&mut self, n_tags: usize) {
        while self.workspaces.len() < n_tags {
            self.workspaces
                .push(Workspace::new(self.workspaces.len() as u32));
        }
        if self.workspaces.len() > n_tags {
            self.workspaces.truncate(n_tags);
        }
        if self.active_ws >= self.workspaces.len() {
            self.active_ws = self.workspaces.len().saturating_sub(1);
        }
    }
}

// ─── Direction ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Dir {
    Next,
    Prev,
    Left,
    Right,
    Up,
    Down,
}

// ─── Layout kind ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayoutKind {
    Column, // niri-style: one or more windows per column, columns side by side
    Grid,   // equal grid
}

impl LayoutKind {
    pub fn from_str(s: &str) -> Self {
        match s {
            "grid" => Self::Grid,
            _ => Self::Column,
        }
    }
    pub fn symbol(&self) -> &'static str {
        match self {
            Self::Column => "[|]",
            Self::Grid => "[#]",
        }
    }
}

// ─── Key actions ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Action {
    Spawn(Vec<String>),
    Kill,
    FocusDir(Dir),
    MoveDir(Dir),
    ToggleFloat,
    ToggleFullscreen,
    SetLayout(LayoutKind),
    CycleLayout,
    GrowCol(i32),    // pixels to grow/shrink column width
    NewColumn,       // move focused window into a new column to the right
    CollapseColumn,  // merge column into previous
    View(usize),     // switch to workspace n
    MoveToWs(usize), // move window to workspace n
    FocusMon(Dir),
    MoveMon(Dir),
    Restart,
    /// Quit immediately (sets `running = false`). No confirmation dialog.
    /// This is not bound to a default key — the Mod4+Shift+Q default shells
    /// out to `maverickctl quit --confirm` — so in practice it is reachable
    /// via IPC (`dispatch quit`) or the TOML config only.
    Quit,
}

// ─── Global state ────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct State {
    pub clients: HashMap<WindowId, Client>,
    pub monitors: Vec<Monitor>,
    pub sel_mon: usize,
    pub focus_serial: u64,
    pub running: bool,
    pub status: String,
}

impl State {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            monitors: Vec::new(),
            sel_mon: 0,
            focus_serial: 0,
            running: true,
            status: String::from("maverick"),
        }
    }

    pub fn mon(&self) -> &Monitor {
        // Defensive: even with debug_assert, avoid panic in release by using get.
        let i = self.sel_mon.min(self.monitors.len().saturating_sub(1));
        &self.monitors[i]
    }
    pub fn mon_mut(&mut self) -> &mut Monitor {
        let i = self.sel_mon.min(self.monitors.len().saturating_sub(1));
        &mut self.monitors[i]
    }

    /// Pick the best window to focus on `mon_idx`'s active workspace. Pure (no X11).
    ///
    /// Order of preference:
    ///   1. a fullscreen/maximized (presentation-overlay) window on the
    ///      workspace, most-recently-focused first — so that closing a tile in
    ///      *peek* mode returns focus to the overlay window instead of leaving
    ///      the user on an invisible tile underneath;
    ///   2. the column-focused window;
    ///   3. the most-recently focused window in the focus stack.
    pub fn best_focus(&self, mon_idx: usize) -> Option<WindowId> {
        let mon = self.monitors.get(mon_idx)?;
        let ws_idx = mon.active_ws;
        if ws_idx >= mon.workspaces.len() {
            return None;
        }
        let overlay = mon.focus_stack.iter().rev().find(|&&w| {
            self.clients
                .get(&w)
                .is_some_and(|c| c.workspace == ws_idx && (c.is_fullscreen() || c.is_maximized()))
        });
        if let Some(&w) = overlay {
            return Some(w);
        }
        let col_win = mon.workspaces[ws_idx].focused_win();
        let from_stack = mon
            .focus_stack
            .iter()
            .rev()
            .find(|&&w| self.clients.get(&w).is_some_and(|c| c.workspace == ws_idx))
            .copied();
        col_win.or(from_stack)
    }

    pub fn mon_at(&self, x: i32, y: i32) -> usize {
        for (i, m) in self.monitors.iter().enumerate() {
            if m.screen.contains(x, y) {
                return i;
            }
        }
        self.sel_mon
    }

    pub fn next_serial(&mut self) -> u64 {
        self.focus_serial += 1;
        self.focus_serial
    }

    pub fn add_client(&mut self, c: Client) {
        let win = c.window;
        self.clients.insert(win, c);
    }

    pub fn remove_client(&mut self, win: WindowId) -> Option<Client> {
        let c = self.clients.remove(&win)?;
        if self.monitors.is_empty() {
            return Some(c);
        }
        // c.monitor may be stale after hotplug (fewer monitors than before).
        // Clamp to avoid panic index out-of-bounds.
        let mon_i = c.monitor.min(self.monitors.len().saturating_sub(1));
        let mon = &mut self.monitors[mon_i];
        mon.focus_stack.retain(|&w| w != win);
        if mon.focused == Some(win) {
            mon.focused = mon.focus_stack.last().copied();
        }
        if c.workspace < mon.workspaces.len() {
            mon.workspaces[c.workspace].remove_window(win);
        }
        Some(c)
    }

    /// Pure workspace rearrangement for `MoveDir` — no X11 calls.
    /// Call this from x11.rs then follow up with arrange/focus.
    /// Returns false if there was nothing to do (float, empty workspace, boundary no-op).
    pub fn apply_move_dir(&mut self, dir: Dir) -> bool {
        if self.monitors.is_empty() {
            return false;
        }
        let mi = self.sel_mon.min(self.monitors.len().saturating_sub(1));
        let ws_i = match self.monitors.get(mi) {
            Some(m) => m.active_ws,
            None => return false,
        };
        // Same 75%-of-workarea sizing as Workspace::add_tiled, so a column
        // created by extracting a window looks the same as one created by
        // opening a new window.
        let workarea_w = self.monitors[mi].workarea.w;
        let focused = match self.monitors[mi].focused {
            Some(w) => w,
            None => return false,
        };

        if self.clients.get(&focused).is_some_and(Client::is_float) {
            return false;
        }

        let (ci, n_cols, col_len) = {
            let ws = &self.monitors[mi].workspaces[ws_i];
            (
                ws.focus.column_idx,
                ws.columns.len(),
                ws.columns
                    .get(ws.focus.column_idx)
                    .map_or(0, |c| c.windows.len()),
            )
        };

        // P3: mutate in-place, no clone
        match dir {
            Dir::Left | Dir::Right => {
                if col_len <= 1 {
                    let ws = &mut self.monitors[mi].workspaces[ws_i];
                    match dir {
                        Dir::Left if ci > 0 => {
                            ws.columns.swap(ci, ci - 1);
                            ws.focus.column_idx = ci - 1;
                        }
                        Dir::Right if ci + 1 < n_cols => {
                            ws.columns.swap(ci, ci + 1);
                            ws.focus.column_idx = ci + 1;
                        }
                        _ => return false,
                    }
                } else {
                    let ws = &mut self.monitors[mi].workspaces[ws_i];
                    ws.remove_window(focused);
                    let insert_pos =
                        (if dir == Dir::Left { ci } else { ci + 1 }).min(ws.columns.len());
                    let new_col_w = (workarea_w as f32 * 0.75) as u32;
                    let mut new_col = Column::new(new_col_w);
                    new_col.windows.push(focused);
                    new_col.focused = 0;
                    ws.columns.insert(insert_pos, new_col);
                    ws.focus.column_idx = insert_pos;
                }
            }
            Dir::Up | Dir::Down => {
                let ws = &mut self.monitors[mi].workspaces[ws_i];
                if let Some(col) = ws.columns.get_mut(ci) {
                    let n = col.windows.len();
                    if n < 2 {
                        return false;
                    }
                    let ri = col.focused;
                    let new_ri = if dir == Dir::Up {
                        (ri + n - 1) % n
                    } else {
                        (ri + 1) % n
                    };
                    col.windows.swap(ri, new_ri);
                    col.focused = new_ri;
                } else {
                    return false;
                }
            }
            _ => return false,
        }
        true
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod reservation_tests {
    use super::*;

    fn mon() -> Monitor {
        Monitor::new(Rect::new(0, 0, 1920, 1080), 9)
    }

    #[test]
    fn top_dock_reserves_top_only() {
        let mut m = mon();
        m.set_reserved_region(0x1001, Edge::Top, 22);
        assert_eq!(
            m.reserved,
            ReservedArea {
                top: 22,
                ..Default::default()
            }
        );
        assert_eq!(m.workarea, Rect::new(0, 22, 1920, 1058));
    }

    #[test]
    fn bottom_dock_reserves_bottom_only() {
        let mut m = mon();
        m.set_reserved_region(0x1001, Edge::Bottom, 30);
        assert_eq!(
            m.reserved,
            ReservedArea {
                bottom: 30,
                ..Default::default()
            }
        );
        assert_eq!(m.workarea, Rect::new(0, 0, 1920, 1050));
    }

    #[test]
    fn two_docks_stack_on_same_edge() {
        // Two top docks (22 + 40) both reserve the top edge.
        let mut m = mon();
        m.set_reserved_region(0x1001, Edge::Top, 22);
        m.set_reserved_region(0x1002, Edge::Top, 40);
        assert_eq!(m.reserved.top, 62);
        assert_eq!(m.workarea, Rect::new(0, 62, 1920, 1018));
    }

    #[test]
    fn removing_external_dock_restores_workarea() {
        let mut m = mon();
        let before = m.workarea;
        m.set_reserved_region(0x1001, Edge::Bottom, 40);
        assert_eq!(m.workarea, Rect::new(0, 0, 1920, 1040));
        assert!(m.remove_reserved_region(0x1001));
        assert_eq!(m.workarea, before);
        // Removing a non-existent owner is a no-op.
        assert!(!m.remove_reserved_region(0x9999));
    }

    #[test]
    fn left_and_right_docks_shrink_width() {
        let mut m = mon();
        m.set_reserved_region(0x1, Edge::Left, 50);
        m.set_reserved_region(0x2, Edge::Right, 60);
        assert_eq!(m.workarea, Rect::new(50, 0, 1810, 1080));
    }

    #[test]
    fn zero_thickness_region_is_removal() {
        let mut m = mon();
        m.set_reserved_region(0x1, Edge::Top, 40);
        assert_eq!(m.reserved.top, 40);
        m.set_reserved_region(0x1, Edge::Top, 0);
        assert_eq!(m.reserved.top, 0);
        assert!(m.reserved.is_empty());
    }
}
