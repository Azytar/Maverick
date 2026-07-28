// maverick/src/types.rs
// Core state — niri-style columnar layout, clean coordinates, no drift.

use std::collections::HashMap;

pub type TagMask = u32;

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
    pub width: u32,           // pixel width of this column
    pub focused: usize,       // index into `windows` that has focus
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
    pub window_idx: usize,
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
            focus: Focus {
                column_idx: 0,
                window_idx: 0,
            },
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

    /// Advance this workspace's layout Column→Monocle→Grid→Column.
    /// Pure state mutation (no X11); the single source of truth shared by the
    /// backend's `do_action` and the core `Engine`.
    pub fn cycle_layout(&mut self) -> LayoutKind {
        self.layout = match self.layout {
            LayoutKind::Column => LayoutKind::Monocle,
            LayoutKind::Monocle => LayoutKind::Grid,
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
            self.focus.window_idx = 0;
        } else {
            // Second window onwards: 75% of the workarea
            let new_col_w = (workarea_w as f32 * 0.75) as u32;
            let mut new_col = Column::new(new_col_w);
            new_col.windows.push(window);
            self.columns.push(new_col);
            self.focus.column_idx = self.columns.len() - 1;
            self.focus.window_idx = 0;
        }
    }

    /// P3: &mut self — no clone needed at call sites
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
            self.focus.window_idx = 0;
        } else if self.focus.column_idx >= self.columns.len() {
            self.focus.column_idx = self.columns.len() - 1;
            let col = &self.columns[self.focus.column_idx];
            self.focus.window_idx = col.focused.min(col.windows.len().saturating_sub(1));
        } else {
            let col = &self.columns[self.focus.column_idx];
            self.focus.window_idx = col.focused.min(col.windows.len().saturating_sub(1));
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
    pub tags: TagMask,
    pub flags: WinFlags,
    pub hints: SizeHints,
    pub monitor: usize,
    pub workspace: usize, // index into Monitor::workspaces
    pub focus_serial: u64,
    pub is_dialog: bool,
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
            tags: 1 << ws,
            flags: WinFlags::default(),
            hints: SizeHints::default(),
            monitor: mon,
            workspace: ws,
            focus_serial: 0,
            is_dialog: false,
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
// backend-agnostic: on X11 the regions come from the internal bar plus each
// external dock's `_NET_WM_STRUT[_PARTIAL]`; a future Wayland backend would fill
// the same regions from layer-shell exclusive zones.

/// Which screen edge a reservation pushes in from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

/// A single trackable reservation. `owner` identifies the source: the internal
/// bar uses `ReservedRegion::INTERNAL_BAR`, external docks use their window id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservedRegion {
    pub owner: WindowId,
    pub edge: Edge,
    /// Thickness in px pushed in from `edge`.
    pub thickness: u32,
}

impl ReservedRegion {
    /// Sentinel owner id for the internal bar (real X11 window ids are never 0).
    pub const INTERNAL_BAR: WindowId = 0;
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
    /// the same edge stack (e.g. internal bar + a top dock).
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
    /// Individual trackable reservations (internal bar + external docks).
    pub reserved_regions: Vec<ReservedRegion>,
    /// Collapsed per-edge totals, derived from `reserved_regions`.
    pub reserved: ReservedArea,
    /// The internal bar's own thickness in px. Authoritative and independent of
    /// `workarea` — do NOT derive it from `screen.h - workarea.h`, because that
    /// difference also includes external docks.
    pub internal_bar_height: u32,
    pub bar_win: Option<WindowId>,
    pub bar_gc: Option<u32>, // GC id
    pub show_bar: bool,
    pub top_bar: bool,
    pub workspaces: Vec<Workspace>,
    pub active_ws: usize,
    pub focused: Option<WindowId>,
    pub focus_stack: Vec<WindowId>,
}

impl Monitor {
    pub fn new(screen: Rect, bar_height: u32, top_bar: bool, n_tags: usize) -> Self {
        let workspaces = (0..n_tags).map(|i| Workspace::new(i as u32)).collect();
        let mut m = Self {
            screen,
            workarea: screen,
            reserved_regions: Vec::new(),
            reserved: ReservedArea::default(),
            internal_bar_height: bar_height,
            bar_win: None,
            bar_gc: None,
            show_bar: true,
            top_bar,
            workspaces,
            active_ws: 0,
            focused: None,
            focus_stack: Vec::with_capacity(16),
        };
        m.sync_internal_bar_region();
        m.recalc_geometry();
        m
    }

    pub fn bar_y(&self) -> i32 {
        if self.top_bar {
            self.screen.y
        } else {
            self.screen.y + self.screen.h as i32 - self.bar_height() as i32
        }
    }

    /// Height of the INTERNAL bar only (0 when hidden). Never includes docks.
    pub fn bar_height(&self) -> u32 {
        if self.show_bar {
            self.internal_bar_height
        } else {
            0
        }
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

    /// Keep the internal bar's own region in sync with `show_bar`/`top_bar`.
    fn sync_internal_bar_region(&mut self) {
        self.reserved_regions
            .retain(|r| r.owner != ReservedRegion::INTERNAL_BAR);
        if self.show_bar && self.internal_bar_height > 0 {
            let edge = if self.top_bar { Edge::Top } else { Edge::Bottom };
            self.reserved_regions.push(ReservedRegion {
                owner: ReservedRegion::INTERNAL_BAR,
                edge,
                thickness: self.internal_bar_height,
            });
        }
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

    /// Toggle the internal bar and recompute geometry. `_bar_h` kept for call-site
    /// compatibility; the authoritative height lives in `internal_bar_height`.
    pub fn recalc_workarea(&mut self, _bar_h: u32) {
        self.sync_internal_bar_region();
        self.recalc_geometry();
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutKind {
    Column,  // niri-style: one or more windows per column, columns side by side
    Monocle, // one window fills workarea
    Grid,    // equal grid
}

impl LayoutKind {
    pub fn from_str(s: &str) -> Self {
        match s {
            "monocle" => Self::Monocle,
            "grid" => Self::Grid,
            _ => Self::Column,
        }
    }
    pub fn symbol(&self) -> &'static str {
        match self {
            Self::Column => "[|]",
            Self::Monocle => "[M]",
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
    ToggleBar,
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
    /// Quit immediately (sets running = false). No confirmation dialog.
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
        let i = self.sel_mon.min(
            self.monitors.len().saturating_sub(1).max(0),
        );
        &self.monitors[i]
    }
    pub fn mon_mut(&mut self) -> &mut Monitor {
        let i = self.sel_mon.min(
            self.monitors.len().saturating_sub(1).max(0),
        );
        &mut self.monitors[i]
    }

    /// Pick the best window to focus on `mon_idx`'s active workspace: the
    /// column-focused window, else the most-recently focused window in the
    /// focus stack that still lives on that workspace. Pure (no X11).
    pub fn best_focus(&self, mon_idx: usize) -> Option<WindowId> {
        let mon = self.monitors.get(mon_idx)?;
        let ws_idx = mon.active_ws;
        if ws_idx >= mon.workspaces.len() {
            return None;
        }
        let col_win = mon.workspaces[ws_idx].focused_win();
        let from_stack = mon
            .focus_stack
            .iter()
            .rev()
            .find(|&&w| {
                self.clients
                    .get(&w)
                    .is_some_and(|c| c.workspace == ws_idx)
            })
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

        if self
            .clients
            .get(&focused)
            .is_some_and(Client::is_float)
        {
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
                            ws.focus.window_idx = 0;
                        }
                        Dir::Right if ci + 1 < n_cols => {
                            ws.columns.swap(ci, ci + 1);
                            ws.focus.column_idx = ci + 1;
                            ws.focus.window_idx = 0;
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
                    ws.focus.window_idx = 0;
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
                    ws.focus.window_idx = new_ri;
                } else {
                    return false;
                }
            }
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod reservation_tests {
    use super::*;

    fn mon(bar_h: u32, top: bool) -> Monitor {
        Monitor::new(Rect::new(0, 0, 1920, 1080), bar_h, top, 9)
    }

    #[test]
    fn internal_top_bar_reserves_top_only() {
        let m = mon(22, true);
        assert_eq!(m.reserved, ReservedArea { top: 22, ..Default::default() });
        assert_eq!(m.workarea, Rect::new(0, 22, 1920, 1058));
        assert_eq!(m.bar_height(), 22);
    }

    #[test]
    fn internal_bottom_bar_reserves_bottom_only() {
        let m = mon(30, false);
        assert_eq!(m.reserved, ReservedArea { bottom: 30, ..Default::default() });
        assert_eq!(m.workarea, Rect::new(0, 0, 1920, 1050));
    }

    #[test]
    fn internal_and_external_stack_on_same_edge() {
        // Internal top bar (22) + an external top dock (40) both reserve the top.
        let mut m = mon(22, true);
        m.set_reserved_region(0x1001, Edge::Top, 40);
        assert_eq!(m.reserved.top, 62);
        assert_eq!(m.workarea, Rect::new(0, 62, 1920, 1018));
    }

    #[test]
    fn removing_external_dock_restores_workarea() {
        let mut m = mon(22, true);
        let before = m.workarea;
        m.set_reserved_region(0x1001, Edge::Bottom, 40);
        assert_eq!(m.workarea, Rect::new(0, 22, 1920, 1018));
        assert!(m.remove_reserved_region(0x1001));
        assert_eq!(m.workarea, before);
        // Removing a non-existent owner is a no-op.
        assert!(!m.remove_reserved_region(0x9999));
    }

    #[test]
    fn bar_height_is_authoritative_not_derived() {
        // Even with a big external dock, bar_height() reports ONLY the internal
        // bar's own height — never screen.h - workarea.h.
        let mut m = mon(22, true);
        m.set_reserved_region(0x1001, Edge::Bottom, 300);
        assert_eq!(m.bar_height(), 22);
        assert_eq!(m.screen.h - m.workarea.h, 322);
    }

    #[test]
    fn left_and_right_docks_shrink_width() {
        let mut m = mon(0, true); // no internal bar
        m.set_reserved_region(0x1, Edge::Left, 50);
        m.set_reserved_region(0x2, Edge::Right, 60);
        assert_eq!(m.workarea, Rect::new(50, 0, 1810, 1080));
    }

    #[test]
    fn hiding_internal_bar_frees_its_reservation() {
        let mut m = mon(22, true);
        m.show_bar = false;
        m.recalc_workarea(22);
        assert_eq!(m.reserved.top, 0);
        assert_eq!(m.workarea, m.screen);
    }

    #[test]
    fn zero_thickness_region_is_removal() {
        let mut m = mon(0, true);
        m.set_reserved_region(0x1, Edge::Top, 40);
        assert_eq!(m.reserved.top, 40);
        m.set_reserved_region(0x1, Edge::Top, 0);
        assert_eq!(m.reserved.top, 0);
        assert!(m.reserved.is_empty());
    }
}
