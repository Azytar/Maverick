// maverick/src/types.rs
// Core state — niri-style columnar layout, clean coordinates, no drift.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::core::wallpaper::{WallpaperMode, WallpaperSource, WallpaperSpec};

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
    /// True when `other` is entirely inside `self`. Used for occlusion culling
    /// (Fase 12): a window fully behind a single opaque window above it is
    /// hidden.
    #[inline]
    pub fn right(&self) -> i32 {
        self.x + self.w as i32
    }
    /// True when `other` is entirely inside `self`. Used for occlusion culling
    /// (Fase 12): a window fully behind a single opaque window above it is
    /// hidden.
    #[inline]
    pub fn contains_rect(&self, other: Rect) -> bool {
        self.x <= other.x
            && self.y <= other.y
            && self.right() >= other.right()
            && self.bottom() >= other.bottom()
    }
    #[inline]
    pub fn bottom(&self) -> i32 {
        self.y + self.h as i32
    }
    /// Smallest rect containing both `self` and `other`. Used for animation
    /// damage (Fase 7): a window that slides from one rect to another must
    /// repaint the union so neither the pixels it left nor the ones it slid
    /// into linger.
    #[inline]
    pub fn union(&self, other: Rect) -> Rect {
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = self.right().max(other.right());
        let y1 = self.bottom().max(other.bottom());
        Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32)
    }
}

// ─── Window flags ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default)]
pub struct WinFlags(u16);
impl WinFlags {
    pub const FLOAT: u16 = 1 << 0;
    pub const FULLSCREEN: u16 = 1 << 1;
    pub const URGENT: u16 = 1 << 2;
    pub const NO_FOCUS: u16 = 1 << 3;
    pub const FIXED: u16 = 1 << 4;
    /// Maximized *vertically* — `_NET_WM_STATE_MAXIMIZED_VERT`. The window's
    /// height (and y) come from the workarea; its width/x stay whatever the
    /// layout gave it. Kept as a separate axis from `MAXIMIZED_H` because EWMH
    /// treats them as two independent states and clients really do request only
    /// one of them (a single `MAXIMIZED` bit silently promoted every vertical
    /// maximize into a full one).
    pub const MAXIMIZED_V: u16 = 1 << 5;
    /// Maximized *horizontally* — `_NET_WM_STATE_MAXIMIZED_HORZ`.
    pub const MAXIMIZED_H: u16 = 1 << 8;
    /// Both axes at once — the "maximize" a user means when pressing Mod4+M.
    pub const MAXIMIZED: u16 = Self::MAXIMIZED_V | Self::MAXIMIZED_H;
    /// Sticky: a float that stays visible on every workspace of its monitor
    /// (never hidden by `hide_offscreen`). Set via a window rule.
    pub const STICKY: u16 = 1 << 6;
    /// Remembers that a window was floating before it entered fullscreen, so
    /// leaving fullscreen can return it to its float (and `saved_geom`) instead
    /// of dropping it back as a tiled column. Set by `ToggleFullscreen`.
    pub const FS_WAS_FLOAT: u16 = 1 << 7;

    #[inline]
    pub fn set(&mut self, f: u16) {
        self.0 |= f;
    }
    #[inline]
    pub fn clear(&mut self, f: u16) {
        self.0 &= !f;
    }
    #[inline]
    pub fn toggle(&mut self, f: u16) {
        self.0 ^= f;
    }
    #[inline]
    pub fn has(&self, f: u16) -> bool {
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

// ─── Column (true scrolling, niri-style) ───────────────────────────────────────
//
// Each workspace has N columns. Every column holds one or more windows
// stacked vertically. The layout engine assigns absolute screen coords
// based on the column's logical offset and the current scroll position.
//
// A column stores its width as a fraction OF THE WORKAREA WIDTH, independent
// of every other column — weights do NOT need to sum to 1.0. This is what
// makes it a true *scrolling* layout rather than a fit-to-screen one: adding,
// growing, or removing a column never resizes its neighbors. Instead, the
// total width of all columns simply grows or shrinks, and `camera` scrolls
// to keep the focused column in view (see `ideal_scroll`).
//
// New columns are inserted at `cfg.column_width` (a fraction of the
// workarea) rather than stealing space from the currently focused column.
//
// This means coordinates are ALWAYS derived from (col_x + scroll, row_y)
// and never stored as mutable state — no drift possible.

#[derive(Debug, Clone)]
pub struct Column {
    pub windows: Vec<WindowId>, // top-to-bottom
    pub weight: f32,            // this column's own width, as a fraction of workarea width
    pub focused: usize,         // index into `windows` that has focus
    /// Accordion boost for THIS column, animated 0→1. The focused column's boost
    /// eases to 1 while the others ease to 0, so changing focus makes the widths
    /// *glide* instead of snapping (bug C10). Replaces the old single global
    /// `Workspace::accordion` scalar, which could only animate on overview
    /// enter/exit and made every focus change a one-frame jump.
    pub boost: f32,
}

impl Column {
    pub fn new(weight: f32) -> Self {
        Column {
            weight,
            ..Default::default()
        }
    }
    pub fn focused_win(&self) -> Option<WindowId> {
        self.windows.get(self.focused).copied()
    }
}

impl Default for Column {
    fn default() -> Self {
        // A column is created because it (or its window) is the focus target,
        // so it starts fully boosted; `tick_animations` eases it back to 0 if it
        // loses focus (bug C10).
        Self {
            windows: Vec::new(),
            weight: 1.0,
            focused: 0,
            boost: 1.0,
        }
    }
}

// ─── Camera (scroll ribbon, spring-damped) ─────────────────────────────────────
//
// 1D camera for the Scroll (niri-style ribbon) layout. `position` is the current
// scroll offset in px (world space -> screen). `target` is where focus wants the
// camera; a second-order spring-damper eases `position` toward it, giving inertia
// without overshoot. It is never the source of truth for geometry
// — `arrange_columns` derives each window's x from it, so there is no drift.

#[derive(Debug, Clone, Copy)]
pub struct Camera {
    pub position: f32,
    pub target: f32,
    pub velocity: f32,
    pub stiffness: f32,
    pub damping: f32,
}

impl Camera {
    pub fn new(pos: f32) -> Self {
        Self {
            position: pos,
            target: pos,
            velocity: 0.0,
            stiffness: 220.0,
            damping: 30.0,
        }
    }
    /// Advance one step of `dt` seconds. Returns true while still moving.
    pub fn step(&mut self, dt: f32) -> bool {
        let disp = self.position - self.target;
        let accel = -self.stiffness * disp - self.damping * self.velocity;
        self.velocity += accel * dt;
        self.position += self.velocity * dt;
        self.velocity.abs() > 0.01 || disp.abs() > 0.5
    }
    /// Snap immediately (no animation) — used on first layout / unmanage.
    pub fn snap(&mut self, pos: f32) {
        self.position = pos;
        self.target = pos;
        self.velocity = 0.0;
    }
}

// ─── Grid snapshot ─────────────────────────────────────────────────────────────
//
// The `Grid` layout is a *pure* geometry engine (see `core::grid`). Its snapshot
// is a *derived cache* — regenerated on every `arrange` — used only to (a) keep
// window motion stable across insertions/removals and (b) give spatial focus /
// move commands the geometry they need without recomputing it. It is never the
// source of truth for geometry (which stays a pure function of the window set +
// workarea); if it is ever stale, at most one frame of suboptimal stability
// results.

/// One window's slot in the current grid arrangement. `rect` is the base
/// (pre-fullscreen-overlay) geometry with the border already subtracted from the
/// content width/height, so it matches the `Rect` stored in `Placements`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridPlacement {
    pub win: WindowId,
    pub rect: Rect,
    pub row: usize,
    pub col: usize,
}

/// The full set of grid placements for a workspace, in stable window order.
#[derive(Debug, Clone, Default)]
pub struct GridSnapshot {
    pub placements: Vec<GridPlacement>,
}

impl GridSnapshot {
    /// The base geometry of `win`, if it is currently tiled in the grid.
    pub fn rect_of(&self, win: WindowId) -> Option<Rect> {
        self.placements
            .iter()
            .find(|p| p.win == win)
            .map(|p| p.rect)
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
    /// Scroll camera (only meaningful in `LayoutKind::Column` / ribbon mode).
    pub camera: Camera,
    pub floats: Vec<WindowId>,
    /// Layout mode for this specific workspace — independent of every other workspace.
    pub layout: LayoutKind,
    /// Semantic-zoom factor for the Overview film-strip (1.0 = normal, <1 = zoomed out).
    pub zoom: f32,
    /// Overview (film-strip zoom-out) mode active for this workspace.
    pub overview: bool,
    /// Semantic-zoom target animated toward by `tick_animations`.
    pub zoom_target: f32,
    /// Viewport display mode (normal vs zoomed-in inspection). Orthogonal to
    /// `overview` and to window fullscreen.
    pub viewport_mode: ViewportMode,
    /// Page-zoom factor when `viewport_mode == Zoomed` (1.0 = no zoom, >1 = the
    /// ribbon is enlarged). Fed into `ribbon_geom`'s `alpha` so columns grow;
    /// there is deliberately no upper clamp (unlike `zoom`'s lower one), so a
    /// value > 1 enlarges instead of shrinking.
    pub page_zoom: f32,
    /// Animated target of `page_zoom`, eased by `tick_animations` (Fase 9/11).
    pub page_zoom_target: f32,
    /// Derived cache of the current `Grid` arrangement (see `GridSnapshot`).
    /// `None` until the first `arrange` runs, or on non-Grid workspaces. Pure
    /// geometry is recomputed from this each frame; it is only consumed for
    /// stability and spatial navigation.
    pub grid_snapshot: Option<GridSnapshot>,
    /// The window currently presented as the **maximize** overlay on this
    /// workspace (`None` when no maximized window owns the overlay). This is the
    /// explicit maximize-overlay owner; it is kept in sync with `Monitor::focused`
    /// + the focused window's maximize flags by `State::sync_presented_maximize`,
    ///   so no read site has to re-derive the maximize-overlay owner from
    ///   `Monitor::focused`. `Monitor::focused` stays purely "the logical focus".
    pub presented_maximize: Option<WindowId>,
}

impl Workspace {
    pub fn new(tag: u32) -> Self {
        Self {
            tag,
            columns: Vec::new(),
            focus: Focus { column_idx: 0 },
            camera: Camera::new(0.0),
            floats: Vec::new(),
            layout: LayoutKind::Column,
            zoom: 1.0,
            overview: false,
            zoom_target: 1.0,
            viewport_mode: ViewportMode::Normal,
            page_zoom: 1.0,
            page_zoom_target: 1.0,
            grid_snapshot: None,
            presented_maximize: None,
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

    /// True-scroll insert: a new window becomes a sibling column inserted to
    /// the RIGHT of the focused column (or as the sole column when the
    /// workspace is empty), at the configured `column_width` (a fraction of the
    /// workarea, 0.1–1.0). The fraction is taken directly — callers pass
    /// `cfg.column_width`; no division happens here (T5).
    pub fn add_tiled(&mut self, window: WindowId, column_width: f32) {
        let w = if column_width <= 0.0 {
            1.0
        } else {
            column_width.clamp(0.1, 1.0)
        };
        if self.columns.is_empty() {
            let mut col = Column::new(1.0); // sole column owns the full workarea width
            col.windows.push(window);
            self.columns.push(col);
            self.focus.column_idx = 0;
        } else {
            let active = self.focus.column_idx.min(self.columns.len() - 1);
            let mut new_col = Column::new(w);
            new_col.windows.push(window);
            new_col.focused = 0;
            self.columns.insert(active + 1, new_col);
            self.focus.column_idx = active + 1;
        }
    }

    /// Guard against degenerate weights (zero/negative from float drift or a
    /// caller that never set one). Since columns are independently sized in
    /// the true-scroll model, this no longer redistributes weight between
    /// columns — it just gives any broken column a sane fallback width.
    pub fn rebalance_weights(&mut self) {
        for col in &mut self.columns {
            if col.weight <= 0.0 {
                col.weight = 0.5;
            }
        }
    }

    /// P3: &mut self — no clone needed at call sites
    /// Locate `win` within the tiled tree and return `(column_idx, row_idx)`.
    /// Used by `State::remove_client` to re-derive the workspace focus pointer
    /// from the logical focus (`mon.focused`) after a window leaves the tree, so
    /// `ws.focus` can never be left pointing at a stale column/row.
    pub fn index_of_window(&self, win: WindowId) -> Option<(usize, usize)> {
        for (ci, col) in self.columns.iter().enumerate() {
            if let Some(ri) = col.windows.iter().position(|&w| w == win) {
                return Some((ci, ri));
            }
        }
        None
    }

    pub fn remove_window(&mut self, win: WindowId) {
        if let Some(fi) = self.floats.iter().position(|&w| w == win) {
            self.floats.remove(fi);
            return;
        }

        for col in &mut self.columns {
            if let Some(wi) = col.windows.iter().position(|&w| w == win) {
                let focused_row = col.focused;
                col.windows.remove(wi);
                if !col.windows.is_empty() {
                    if wi < focused_row {
                        // A row before the focused one was removed: the focused
                        // row shifts down by one, so the focus pointer must shift
                        // with it. Without this, `col.focused` would silently point
                        // at a *different* window in the same column.
                        col.focused = col.focused.saturating_sub(1);
                    } else if wi == focused_row {
                        // The focused window itself was removed: clamp to the new
                        // tail. The exact window that takes its place is
                        // re-derived from `mon.focused` by `State::remove_client`.
                        col.focused = col.focused.min(col.windows.len() - 1);
                    }
                }
                break;
            }
        }

        self.cleanup_empty_columns();
    }

    /// P3: &mut self — no clone needed at call sites
    pub fn cleanup_empty_columns(&mut self) {
        let target = self.focus.column_idx;
        // How many dropped columns sat strictly *before* the focused column? Each
        // one shifts the focused column's index down by one, so the focus pointer
        // must shift by the same amount — not just be clamped. A bare clamp (the
        // old behaviour) only corrected the case where the focused column itself
        // was dropped, leaving the focus on whatever column happened to occupy the
        // clamped index after an earlier-column removal — which mis-centered the
        // camera (`ideal_scroll`) and let `best_focus`/`focused_win` steal focus
        // to a neighbour.
        let removed_before = self.columns[..target.min(self.columns.len())]
            .iter()
            .filter(|c| c.windows.is_empty())
            .count();

        let had = self.columns.len();
        self.columns.retain(|col| !col.windows.is_empty());
        let dropped = had - self.columns.len();

        if self.columns.is_empty() {
            self.focus.column_idx = 0;
        } else {
            // Shift left by every dropped column that was before the focus, then
            // clamp defensively.
            let new_idx = target.saturating_sub(removed_before);
            self.focus.column_idx = new_idx.min(self.columns.len() - 1);
        }

        // Removing a column leaves the remaining weights short of 1.0. In the
        // true-scroll model each column's width is independent, so the total
        // simply shrinks; but a degenerate/negative weight would break geometry,
        // so repair any non-positive weight to a sane fallback. (This does NOT
        // re-normalize the survivors — see `rebalance_weights`.)
        if dropped > 0 {
            self.rebalance_weights();
        }
    }

    /// Drag-and-drop-to-tile: insert `win` into column `ci` at `pos` and make it
    /// the focused row of that column. `pos` is clamped to the column length so
    /// an `append` (`pos == windows.len()`) is valid. Also points the
    /// workspace's focused column at `ci`. Pure (no X11) so the backend's
    /// `on_button_release` and the unit tests share one source of truth.
    pub fn drop_into_column(&mut self, ci: usize, win: WindowId, pos: usize) {
        if ci >= self.columns.len() {
            return;
        }
        let cws = &mut self.columns[ci];
        let pos = pos.min(cws.windows.len());
        cws.windows.insert(pos, win);
        cws.focused = pos;
        self.focus.column_idx = ci;
    }
}

// ─── Fullscreen policy ────────────────────────────────────────────────────────
//
// A window's fullscreen *state* ("is it fullscreen right now?") is
// `WinFlags::FULLSCREEN`. Its fullscreen *policy* is a different question —
// "what should Maverick do when this window asks for fullscreen?" — and is set
// once from a window rule, never toggled at runtime. Keeping the two apart is
// what lets Firefox's F11 be refused while `Mod4+F` still works on the very
// same window.

/// What Maverick does with a window's fullscreen requests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FullscreenPolicy {
    /// Fullscreen behaves like everywhere else: in `LayoutKind::Column` the
    /// window becomes a screen-wide column of the scrolling ribbon, in `Grid`
    /// it is a pinned overlay.
    #[default]
    Normal,
    /// Refuse fullscreen requests that come *from the client* (an EWMH
    /// `_NET_WM_STATE_FULLSCREEN` client message — which is what a browser's
    /// F11 sends). The user's own `Mod4+F` still works and still produces a
    /// normal tiled fullscreen: this rejects the app's opinion, not the user's.
    ///
    /// This is the runtime counterpart of `Rule::ignore_initial_state`, which
    /// only ever fires once, at map time.
    Deny,
    /// Real, exclusive fullscreen: the window leaves the ribbon entirely and is
    /// presented as an overlay covering `mon.screen` in *any* layout, with
    /// `_NET_WM_BYPASS_COMPOSITOR` asking the compositor to step aside. This is
    /// the mode for games and video players that own their own vsync — Maverick
    /// does not touch their frame pacing, it just gets out of the way.
    True,
}

// ─── Fullscreen snapshot ────────────────────────────────────────────────────
//
// `saved_geom` (a single `Rect`) was the original "remember the pre-fullscreen
// geometry" store, but it is fragile: `set_maximized` and `set_fullscreen` both
// write it, so a window that is maximized *while fullscreen* clobbers the saved
// float rect, and leaving fullscreen then restores the wrong geometry (plan
// 1786564084575, Fase 3). The snapshot captures the window's *prevailing mode*
// together with the exact rect it had just before entering fullscreen, so
// leaving fullscreen can restore the prior state verbatim instead of
// reconstructing it from the (shared, mutable) `saved_geom`.

/// The presentation mode a window was in immediately before it entered
/// fullscreen. Used to decide what "leave fullscreen" must restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowMode {
    /// A free-floating window laid out from `client.geom`.
    Float,
    /// A tiled column of the ribbon.
    Tiled,
    /// A maximized (workarea-filling) overlay window.
    Maximized,
}

/// Exact pre-fullscreen state, captured once on enter and applied verbatim on
/// leave. Replaces the overloaded `saved_geom` for fullscreen transitions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FullscreenSnapshot {
    pub prior: WindowMode,
    pub rect: Rect,
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
    /// Observability-only mirror of the last *desired* rect this client was
    /// arranged to (the `DesiredState` rect for it). Written by the render
    /// reconcile path; NEVER read for layout/focus/overlay decisions. Fase 8.
    pub last_desired: Option<Rect>,
    /// Observability-only mirror of the last *real* geometry the client reported
    /// back via `ConfigureNotify` (X11 Real). Written by the events convergence
    /// path; NEVER read for layout/focus/overlay decisions. Fase 8.
    pub last_reported: Option<Rect>,
    pub is_unmanaged: bool,
    pub wants_input: bool,
    pub wm_hidden: bool,
    /// Forces the next `apply_geom` to re-emit its `ConfigureWindow` even when
    /// the computed rect equals `geom`.
    ///
    /// `apply_geom` skips windows whose geometry did not change, which is what
    /// keeps `arrange` cheap. But a *state* transition (entering/leaving
    /// fullscreen or maximized) can produce the very same rect while the window
    /// still has to be reconfigured — the border width changed, or the client
    /// resized itself behind our back. This used to be faked by stomping
    /// `geom` with a `Rect::default()` sentinel, which also collapsed floats to
    /// 0×0 whenever the restore path missed a case. The flag says the same
    /// thing without lying about the geometry; `apply_geom` clears it.
    pub geometry_dirty: bool,
    /// Fullscreen transition snapshot: the exact mode + rect the window had
    /// *before* it entered fullscreen, captured by `apply_fullscreen_topology`
    /// and applied verbatim when it leaves. This is the robust replacement for
    /// using the shared `saved_geom` to remember fullscreen geometry (see
    /// `FullscreenSnapshot`). `None` when the window is not in a fullscreen
    /// transition.
    pub fs_snapshot: Option<FullscreenSnapshot>,
    /// What to do when this window asks for fullscreen. Comes from a window
    /// rule (`deny_fullscreen` / `true_fullscreen`) and never changes at
    /// runtime — it is policy, not state, so it deliberately lives here rather
    /// than as another `WinFlags` bit.
    pub fullscreen_policy: FullscreenPolicy,
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
            last_desired: None,
            last_reported: None,
            is_unmanaged: false,
            wants_input: true,
            wm_hidden: false,
            geometry_dirty: false,
            fs_snapshot: None,
            fullscreen_policy: FullscreenPolicy::Normal,
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
        // Both axes must be on. `WinFlags::MAXIMIZED` is the union of the two
        // axis bits, and `has()` tests bit *overlap*, so `has(MAXIMIZED)` would
        // be true for a single axis. A window is only "maximized" (filling the
        // workarea as an overlay) when V *and* H are both set.
        self.is_maximized_v() && self.is_maximized_h()
    }
    /// Maximized on the vertical axis (`_NET_WM_STATE_MAXIMIZED_VERT`).
    #[inline]
    pub fn is_maximized_v(&self) -> bool {
        self.flags.has(WinFlags::MAXIMIZED_V)
    }
    /// Maximized on the horizontal axis (`_NET_WM_STATE_MAXIMIZED_HORZ`).
    #[inline]
    pub fn is_maximized_h(&self) -> bool {
        self.flags.has(WinFlags::MAXIMIZED_H)
    }
    #[inline]
    pub fn no_focus(&self) -> bool {
        self.flags.has(WinFlags::NO_FOCUS)
    }
    #[inline]
    pub fn is_sticky(&self) -> bool {
        self.flags.has(WinFlags::STICKY)
    }
    /// True when this window's fullscreen is the exclusive, out-of-ribbon kind
    /// (policy `True`) — the one that covers `mon.screen` in every layout.
    #[inline]
    pub fn is_true_fullscreen(&self) -> bool {
        self.fullscreen_policy == FullscreenPolicy::True
    }
    /// True when the window is fullscreen *and* uses the exclusive overlay mode.
    /// This is the condition `core::present`, `stack_overlay` and `best_focus`
    /// share so the three never disagree about who is on top.
    #[inline]
    pub fn is_fullscreen_overlay(&self) -> bool {
        self.is_fullscreen() && self.is_true_fullscreen()
    }
    /// True when the client's own fullscreen requests must be refused.
    #[inline]
    pub fn denies_fullscreen(&self) -> bool {
        self.fullscreen_policy == FullscreenPolicy::Deny
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
    /// Set when this monitor's window geometry changed and its cached live
    /// placements (used by the GLX compositor) must be recomputed. Cleared by
    /// the frame loop after it re-projects the monitor. Starts `true` so the
    /// first frame projects every monitor.
    pub layout_dirty: bool,
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
            layout_dirty: true,
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

    /// Replace *every* region owned by `owner` with `regions` (bug B4). A single
    /// dock may reserve several edges at once (e.g. `top` + `left`), and calling
    /// `set_reserved_region` once per edge would erase the previous one, so all
    /// edges are written together. Passing an empty list clears the owner.
    pub fn set_reserved_regions(&mut self, owner: WindowId, regions: &[(Edge, u32)]) {
        self.reserved_regions.retain(|r| r.owner != owner);
        for &(edge, thickness) in regions {
            if thickness > 0 {
                self.reserved_regions.push(ReservedRegion {
                    owner,
                    edge,
                    thickness,
                });
            }
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

// ─── Viewport mode ───────────────────────────────────────────────────────────
//
// Viewport is a *display-state* axis of the workspace, orthogonal to both the
// window-level fullscreen (`WinFlags::FULLSCREEN`) and the Overview film-strip
// zoom-out. `Zoomed` enlarges the ribbon (alpha > 1, see `core::layout::
// ribbon_geom`) so the user can inspect a column up close; `PageSnap` then
// scrolls the camera by one screen-width. It is deliberately *not* called
// "fullscreen" — fullscreen is a window/EWMH state, this is a workspace view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewportMode {
    #[default]
    Normal,
    Zoomed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Spawn(Vec<String>),
    Kill,
    FocusDir(Dir),
    MoveDir(Dir),
    ToggleFloat,
    ToggleFullscreen,
    /// Toggle the maximized (workarea-filling, border 0) presentation state of
    /// the focused window. Like fullscreen but respects reserved regions and is
    /// only presented while the window is focused (the "peek" overlay in
    /// `core::present`). Previously only reachable via a client's
    /// `_NET_WM_STATE` request, so the keyboard path was missing (bug C18).
    ToggleMaximize,
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
    /// Toggle the Overview (semantic-zoom film-strip) mode for the active workspace.
    ToggleOverview,
    /// Move the selection left/right while in Overview (enters Overview if not active).
    OverviewNav(Dir),
    /// Drop into the currently selected column, leaving Overview (zoom back to 1.0).
    OverviewEnter,
    /// Enlarge/shrink the workspace viewport (zoom in/out). Positive `f32` zooms
    /// in, negative zooms out; enters `ViewportMode::Zoomed` and animates the
    /// `page_zoom` spring (see `Workspace::page_zoom_target`). This is display
    /// state, not window fullscreen.
    ViewportZoom(f32),
    /// Scroll the camera by one screen-width in the given direction (a "page"
    /// of the zoomed ribbon). Reuses `ideal_scroll`/`camera` — no focus change.
    PageSnap(Dir),
    /// Native wallpaper control (set/clear/mode). Pure State mutation in the
    /// core; the backend uploads/draws the GPU texture. Never affects focus,
    /// stacking, input, layout or geometry.
    Wallpaper(WallpaperCmd),
}

/// Imperative sub-verbs of the `wallpaper` action. Paths with spaces are
/// preserved verbatim (the caller joins the rest of the line before handing it
/// here), so a wallpaper at `/home/u/My Pic.png` works unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WallpaperCmd {
    /// Set the wallpaper to the image/shader at `PathBuf` (source type is
    /// inferred from the extension; see `WallpaperSpec::source_for_path`).
    Set(PathBuf),
    /// Clear the native wallpaper, falling back to the legacy root pixmap.
    Clear,
    /// Change only the mapping mode of the current source.
    Mode(WallpaperMode),
}

// ─── Deferred focus ───────────────────────────────────────────────────────────
/// A deferred focus request created while an overlay (fullscreen/maximize owner)
/// is presented. Unlike the old per-`Workspace` `pending_focus: Option<WindowId>`,
/// this carries the *context* it was created under: the exact `monitor`/`workspace`
/// the deferral belongs to and the `owner` overlay that created it. Carrying the
/// context means a deferred window is never orphaned when the overlay is torn down
/// on a non-active workspace or a non-selected monitor — the deferral is keyed on
/// its OWN `monitor`/`workspace`, and a live overlay that did not create it cannot
/// consume a deferral it does not own. There is at most one deferral at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingFocus {
    /// The window that should receive focus once the overlay is dismissed.
    pub window: WindowId,
    /// The overlay that created this deferral (its presented owner at creation).
    pub owner: WindowId,
    /// The monitor the deferral is bound to.
    pub monitor: usize,
    /// The workspace the deferral is bound to.
    pub workspace: usize,
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
    /// The window the X server currently reports as having the input focus
    /// (`GetInputFocus`), mirrored from `FocusIn`/`FocusOut` events. This is the
    /// in-memory truth of the *real* X focus, distinct from `mon.focused` (the
    /// WM's logical intent) and the painted border (visual). `reconcile_focus`
    /// keeps all three in lock-step; without it an external `XSetInputFocus`
    /// (popup/dialog/Wine) silently desyncs logical vs real focus.
    pub x11_input_focus: Option<WindowId>,
    /// Deferred focus request with explicit context (see `PendingFocus`). This is
    /// the single global slot; the deferral names the exact `monitor`/`workspace`
    /// it belongs to and the `owner` overlay that created it, so it is never
    /// orphaned when the overlay tears down on a non-active workspace or a
    /// non-selected monitor (invariant 8c). `None` when there is nothing pending.
    pub pending_focus: Option<PendingFocus>,
    /// Transient windows that mapped before their `WM_TRANSIENT_FOR` parent
    /// was managed. Each entry is the child's id; its desired parent is already
    /// stored on `Client::transient_parent`. Once the parent shows up,
    /// `relink_pending_transients` (x11/backend) moves the child onto the
    /// parent's monitor/workspace and re-floats it, instead of leaving the
    /// popup stranded on whatever monitor happened to be focused at map time.
    pub pending_transients: Vec<WindowId>,
    /// Native wallpaper configuration (source + mode). Pure `State` data — the
    /// compositor reads it and uploads/draws the GPU texture; it never affects
    /// focus, stacking, input, layout or window geometry.
    pub wallpaper: WallpaperSpec,
    /// Monotonic revision of `wallpaper`. Bumped on every `SetWallpaper` so the
    /// compositor knows whether it must re-decode/re-upload the texture without
    /// re-decoding every frame (criterion #5).
    pub wallpaper_rev: u64,
}

impl State {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            monitors: Vec::new(),
            sel_mon: 0,
            focus_serial: 0,
            running: false,
            status: String::new(),
            pending_transients: Vec::new(),
            x11_input_focus: None,
            pending_focus: None,
            wallpaper: WallpaperSpec::default(),
            wallpaper_rev: 0,
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
    ///   1. a presentation-overlay window on the workspace, most-recently-focused
    ///      first — so that closing a tile in *peek* mode returns focus to the
    ///      overlay window instead of leaving the user on an invisible tile
    ///      underneath. The candidate set mirrors `core::present` exactly:
    ///      fullscreen counts when it is actually presented as an overlay (in
    ///      `Grid`, or in any layout under `FullscreenPolicy::True`) — in the
    ///      `Column` ribbon a fullscreen window is a plain scrolling tile and
    ///      gets no special treatment — and maximized only counts while it is
    ///      already the monitor's focused window. Otherwise a maximized window
    ///      sitting in the background would grab the focus on
    ///      `ViewWorkspace`/`MoveToWorkspace`/`FocusMonitor` and, because
    ///      `present` then presents whatever is focused, unexpectedly blow
    ///      itself up to fill the workarea;
    ///   2. the column-focused window;
    ///   3. the most-recently focused window in the focus stack.
    ///
    /// Recompute [`Workspace::presented_maximize`] for the active workspace of
    /// `mon_idx`. The maximize overlay is owned by exactly the monitor's focused
    /// window when that window is maximized (`is_maximized_v() || is_maximized_h()`)
    /// on the active workspace. This is the *single* writer that turns
    /// `Monitor::focused` + the client's maximize flags into the explicit
    /// `presented_maximize` field, so no read site has to infer the
    /// maximize-overlay owner from `mon.focused`.
    pub fn sync_presented_maximize(&mut self, mon_idx: usize) {
        if mon_idx >= self.monitors.len() {
            return;
        }
        let (ws_idx, owner) = {
            let mon = &self.monitors[mon_idx];
            let ws_idx = mon.active_ws;
            let owner = mon.focused.filter(|&w| {
                self.clients.get(&w).is_some_and(|c| {
                    c.workspace == ws_idx && (c.is_maximized_v() || c.is_maximized_h())
                })
            });
            (ws_idx, owner)
        };
        if ws_idx < self.monitors[mon_idx].workspaces.len() {
            self.monitors[mon_idx].workspaces[ws_idx].presented_maximize = owner;
        }
        // A maximize overlay is only ever presented on the ACTIVE workspace (see
        // `core::present` and `presented_overlay_owner`, which only read the
        // active workspace). Clear any stale `presented_maximize` entry on the
        // OTHER workspaces of this monitor, so a window un-maximized on a
        // non-active workspace cannot leave a dangling maximize-overlay owner
        // that trips invariant #9 when that workspace is later activated.
        for (i, ws) in self.monitors[mon_idx].workspaces.iter_mut().enumerate() {
            if i != ws_idx {
                ws.presented_maximize = None;
            }
        }
    }

    pub fn best_focus(&self, mon_idx: usize) -> Option<WindowId> {
        // A pending overlay owns the focus: the window currently presented as the
        // overlay (fullscreen owner, or the focused maximized window) is what the
        // user is looking at, so it is the best focus. Delegating to
        // `presented_overlay_owner` removes the duplicate re-derivation of overlay
        // semantics (invariant 9b) that the previous inline version had.
        if let Some(o) = self.presented_overlay_owner(mon_idx) {
            return Some(o);
        }
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
            .find(|&&w| self.clients.get(&w).is_some_and(|c| c.workspace == ws_idx))
            .copied();
        col_win.or(from_stack)
    }

    /// The single, canonical definition of "which window currently OWNS the
    /// presented overlay" on `mon_idx`'s active workspace. Returns `None` when
    /// there is no real overlay. A window is a real overlay only when:
    ///   1. it is fullscreen AND (`Grid` layout OR `FullscreenPolicy::True`); a
    ///      `Column`-layout Normal-policy fullscreen is just a ribbon tile and
    ///      is explicitly NOT an overlay; OR
    ///   2. it is the *focused* maximized window (`presented_maximize` owner).
    ///
    /// This MUST stay equivalent to `core::present`, `render::stack_overlay` and
    /// `best_focus`. `manage` used to duplicate this with a weaker
    /// `is_fullscreen() || is_maximized_*` condition.
    ///
    /// IMPORTANT: this is **NOT** the same as "covering fullscreen" used by
    /// pointer hit-testing. A `Column`-layout fullscreen is *covering* (it paints
    /// a ribbon tile over the screen) but is **not** returned here — and a
    /// `presented_maximize` window **is** returned here but is **not**
    /// `is_fullscreen()` at all. Use [`State::covering_fullscreen_window`] for the
    /// covering predicate and never substitute `Client::is_fullscreen()` for
    /// either, since `is_fullscreen()` is true for both and would silently merge
    /// the two disjoint concepts.
    pub fn presented_overlay_owner(&self, mon_idx: usize) -> Option<WindowId> {
        let ws_idx = self.monitors.get(mon_idx)?.active_ws;
        self.presented_overlay_owner_in(mon_idx, ws_idx)
    }

    /// The canonical overlay owner on an EXPLICIT workspace `ws_idx` of
    /// `mon_idx`. Shared by [`State::presented_overlay_owner`] (which passes the
    /// monitor's active workspace) and the core EWMH `_NET_ACTIVE_WINDOW` policy
    /// (which must test the *requesting* window's own (monitor, workspace), not
    /// necessarily the active one).
    pub(crate) fn presented_overlay_owner_in(
        &self,
        mon_idx: usize,
        ws_idx: usize,
    ) -> Option<WindowId> {
        let mon = self.monitors.get(mon_idx)?;
        let ws = mon.workspaces.get(ws_idx)?;
        // A window that is *both* fullscreen and maximized is owned exclusively
        // through the maximize branch below (so it agrees with
        // `presented_maximize`); the fullscreen branch only matches a *purely*
        // fullscreen window. Without this guard the two branches could name
        // different windows and trip invariant #9b ("overlay owner /
        // presented_maximize mismatch"). `core::present` still presents *every*
        // fullscreen window regardless of focus; this helper only names the one
        // that owns the overlay for focus/stacking decisions.
        let fs_overlay = mon.focus_stack.iter().rev().find(|&&w| {
            self.clients.get(&w).is_some_and(|c| {
                c.workspace == ws_idx
                    && c.is_fullscreen()
                    && !c.is_maximized()
                    && (ws.layout == LayoutKind::Grid || c.is_true_fullscreen())
            })
        });
        if let Some(&w) = fs_overlay {
            return Some(w);
        }
        if let Some(w) = ws.presented_maximize {
            if self.clients.get(&w).is_some_and(|c| {
                c.workspace == ws_idx && (c.is_maximized_v() || c.is_maximized_h())
            }) {
                return Some(w);
            }
        }
        None
    }

    /// True iff `self.pending_focus` (if set) names an owner that is still a
    /// currently-presented overlay on the deferral's (monitor, workspace) — i.e.
    /// the EXACT `#8c` condition. This is the single shared definition used by
    /// BOTH `check_invariants` (#8c) and the post-command reconciliation hook
    /// (`reconcile_pending_focus_after_transition`) so they can never disagree.
    pub(crate) fn pending_focus_owner_presented(&self) -> bool {
        let pf = match self.pending_focus {
            Some(p) => p,
            None => return false,
        };
        self.monitors.get(pf.monitor).is_some_and(|m| {
            let focused = m.focused;
            m.workspaces.get(pf.workspace).is_some_and(|ws| {
                self.clients.get(&pf.owner).is_some_and(|c| {
                    c.monitor == pf.monitor
                        && c.workspace == pf.workspace
                        && (c.is_fullscreen()
                            && (ws.layout == LayoutKind::Grid || c.is_true_fullscreen())
                            || (c.is_maximized_v() || c.is_maximized_h())
                                && focused == Some(pf.owner))
                })
            })
        })
    }

    /// The single, canonical definition of "which window is the *covering
    /// fullscreen*" on `mon_idx`'s active workspace — i.e. the `Column`-layout
    /// fullscreen tile that currently paints a ribbon tile covering the screen
    /// (used by pointer hit-testing and `render::stack_overlay`'s 2-bis case to
    /// decide which window is under the cursor).
    ///
    /// THIS IS DELIBERATELY DIFFERENT FROM [`State::presented_overlay_owner`]:
    ///   * a `Column` fullscreen is a *covering* fullscreen but is **NOT** an
    ///     overlay owner — in the `Column` ribbon a fullscreen window is just a
    ///     scrolling participant, never an out-of-ribbon overlay
    ///     (`core::present` / `presented_overlay_owner` exclude it on purpose);
    ///   * a `presented_maximize` window **IS** the overlay owner but is **NOT**
    ///     a covering fullscreen (it is not `is_fullscreen()` in the `LayoutKind`
    ///     sense at all);
    ///   * a `Grid` / `FullscreenPolicy::True` fullscreen **IS** the overlay owner
    ///     but is **NOT** a covering fullscreen (it is an exclusive overlay, not
    ///     a ribbon tile — it never joins the Column ribbon).
    ///
    /// Because the `Column` case makes the three categories disjoint, **never**
    /// substitute `Client::is_fullscreen()` for either predicate: `is_fullscreen()`
    /// is true for *all* of the above (Column, Grid and True), so it conflates
    /// "covering" with "overlay owner" and silently changes behaviour. Read the
    /// concrete predicate you actually mean.
    ///
    /// This helper only names *which* window covers the screen; the runtime
    /// raise/drop transition in `stack_overlay` additionally gates on the camera
    /// and zoom being settled. It depends only on `fs_ctx` (the focused
    /// fullscreen column's window) and must stay equivalent to
    /// `fs_ctx(...).win`.
    pub fn covering_fullscreen_window(&self, mon_idx: usize) -> Option<WindowId> {
        let mon = self.monitors.get(mon_idx)?;
        let ws = mon.workspaces.get(mon.active_ws)?;
        // A covering fullscreen is, by definition, a Column-ribbon participant.
        if ws.layout != LayoutKind::Column || ws.overview {
            return None;
        }
        let fs = crate::core::layout::fs_ctx(&self.clients, ws, mon.screen);
        fs.win
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

    /// Read-only view of the wallpaper for the compositor: source, mode, and the
    /// current revision. Pure query — never mutates `State`.
    pub fn wallpaper_layer(&self) -> (WallpaperSource, WallpaperMode, u64) {
        (
            self.wallpaper.source.clone(),
            self.wallpaper.mode,
            self.wallpaper_rev,
        )
    }

    pub fn remove_client(&mut self, win: WindowId) -> Option<Client> {
        let c = self.clients.remove(&win)?;
        // Drop every transient reference to the window that just died, so the
        // ownership graph never names a client that is gone (Riesgo 5).
        //
        //   * a surviving child whose `transient_parent` was `win` is re-parented
        //     to `None` — it becomes a plain float instead of a popup owned by a
        //     ghost. Readers (`render::transient_of`, `decide_manage_focus`,
        //     `relink_pending_transients`) already treat an unknown parent as "no
        //     parent", so behaviour is unchanged; what changes is that the stale
        //     id can no longer be resurrected by X11 *XID reuse* — a brand-new,
        //     unrelated window that happens to get the recycled id would
        //     otherwise inherit these orphans as its popups (raised above its
        //     overlay, relinked onto its monitor/workspace).
        //   * `win` itself (and any child it just orphaned) is dropped from the
        //     deferred-transient queue; that queue is only drained on the next
        //     `manage()`, so without this a destroyed child stayed referenced
        //     there indefinitely.
        for other in self.clients.values_mut() {
            if other.transient_parent == Some(win) {
                other.transient_parent = None;
            }
        }
        let clients = &self.clients;
        self.pending_transients
            .retain(|w| clients.get(w).is_some_and(|c| c.transient_parent.is_some()));
        if self.monitors.is_empty() {
            return Some(c);
        }
        // Clear any stale `presented_maximize` references to this window across
        // *every* monitor/workspace, not just the one the client currently
        // points at. A window that was moved away (MoveWindowToMonitor /
        // MoveToWorkspace) may still be named as the maximize-overlay owner on a
        // former monitor/workspace; leaving that dangling entry triggers
        // invariant #9 ("presented_maximize ... not in clients") once the window
        // is destroyed.
        for mon in &mut self.monitors {
            for ws in &mut mon.workspaces {
                if ws.presented_maximize == Some(win) {
                    ws.presented_maximize = None;
                }
            }
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
            let ws_i = c.workspace;
            mon.workspaces[ws_i].remove_window(win);
            // A deferred (pending) focus that pointed at this window — either
            // because this window was the *target* (`pf.window`) or the *owner*
            // overlay (`pf.owner`) — is now dangling and must be dropped. (When the
            // owner overlay dies, the deferred window is re-focused by the caller's
            // unmanage/consume path rather than staying orphaned.)
            if let Some(pf) = self.pending_focus {
                if pf.window == win || pf.owner == win {
                    self.pending_focus = None;
                }
            }
            // Keep the workspace focus pointer (column + row) in lock-step with
            // the logical focus (`mon.focused`). `remove_window` only shifts/
            // clamps the column index relative to the surviving tree; it does not
            // know which window is logically focused. Re-deriving the pointer from
            // `mon.focused` here is the single source of truth for "what is
            // focused": without it, removing a column before the focused one left
            // `ws.focus.column_idx` on a different column, so the camera
            // (`ideal_scroll` reads `focus.column_idx`) centered the wrong column
            // and `best_focus`/`focused_win` returned a neighbour — focus silently
            // jumped to the wrong window on close-before-focus, close-visible and
            // close-offscreen (invariant: focus must not depend on visibility).
            if let Some(fw) = mon.focused {
                if let Some((ci, ri)) = mon.workspaces[ws_i].index_of_window(fw) {
                    mon.workspaces[ws_i].focus.column_idx = ci;
                    mon.workspaces[ws_i].columns[ci].focused = ri;
                }
            }
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
        // Spring-split ratio used when extracting a window into its own
        // column. No `Cfg` here, so a balanced split keeps the node weights
        // normalized (the caller can re-tune via grow/shrink afterwards).
        let focused = match self.monitors[mi].focused {
            Some(w) => w,
            None => return false,
        };

        if self.clients.get(&focused).is_some_and(Client::is_float) {
            return false;
        }

        // The `Grid` layout is one window per column, so the column-based move
        // below would only shuffle 1-window columns. Instead move the focused
        // window by swapping it with its *geometric* neighbour in the flat
        // stable order, then rebuild the single-window columns in that order.
        if self.monitors[mi].workspaces[ws_i].layout == LayoutKind::Grid {
            return self.apply_move_dir_grid(dir);
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
                    let ratio = 0.5;
                    let src_w = ws.columns[ci].weight;
                    ws.remove_window(focused); // column keeps `src_w` (still non-empty)
                    let index_in_ws = if dir == Dir::Left { ci } else { ci + 1 };
                    let insert_pos = index_in_ws.min(ws.columns.len());
                    // Spring-split the source column: it keeps `ratio` of its
                    // weight, the extracted window takes the rest.
                    ws.columns[insert_pos.min(ci)].weight = src_w * ratio;
                    let mut new_col = Column::new(src_w * (1.0 - ratio));
                    new_col.windows.push(focused);
                    new_col.focused = 0;
                    ws.columns.insert(insert_pos, new_col);
                    ws.focus.column_idx = insert_pos;
                    ws.rebalance_weights();
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

    /// `Grid`-layout move: swap the focused window with its geometric neighbour
    /// in the flat stable window order, then rewrite `ws.columns` as one-window
    /// columns in that new order. Keeps `ws.focus.column_idx` on the moved
    /// window. Geometry is taken from the render-kept `grid_snapshot` so the
    /// neighbour is the real on-screen one; if absent, a gap-free arrangement is
    /// used (direction is scale-invariant).
    fn apply_move_dir_grid(&mut self, dir: Dir) -> bool {
        let mi = self.sel_mon.min(self.monitors.len().saturating_sub(1));
        let ws_i = self.monitors[mi].active_ws;
        let focused = {
            // Use the WM's logical focused window (mon.focused), not
            // `ws.focused_win()` — in Grid every window is its own single-window
            // column, so `ws.focus.column_idx` may point at a different window.
            let Some(f) = self.monitors[mi].focused else {
                return false;
            };
            f
        };
        let (wins, placements) = {
            let ws = &self.monitors[mi].workspaces[ws_i];
            let mon = &self.monitors[mi];
            let wins: Vec<WindowId> = ws
                .columns
                .iter()
                .flat_map(|c| c.windows.iter().copied())
                .collect();
            let placements: Vec<(WindowId, Rect)> = if let Some(s) = &ws.grid_snapshot {
                s.placements.iter().map(|p| (p.win, p.rect)).collect()
            } else {
                crate::core::grid::arrange(&wins, mon.workarea, 0, 0, None).0
            };
            (wins, placements)
        };
        let Some(nb) = crate::core::grid::neighbor(&placements, focused, dir) else {
            return false;
        };

        let mut order = wins;
        if let (Some(fi), Some(ni)) = (
            order.iter().position(|&w| w == focused),
            order.iter().position(|&w| w == nb),
        ) {
            order.swap(fi, ni);
        }

        let area = self.monitors[mi].workarea;
        let ws = &mut self.monitors[mi].workspaces[ws_i];
        ws.columns.clear();
        for w in &order {
            ws.columns.push(Column {
                windows: vec![*w],
                focused: 0,
                weight: 1.0,
                boost: 1.0,
            });
        }
        ws.focus.column_idx = order.iter().position(|&w| w == focused).unwrap_or(0);
        ws.cleanup_empty_columns();
        // Refresh the grid snapshot so the next `arrange` (which uses it as the
        // stability anchor) preserves this explicit reorder instead of reverting
        // it back to the pre-move layout. Geometry isn't needed for stability
        // beyond the (row, col) slots, so a gap/border-free arrange suffices.
        let (_, snap) = crate::core::grid::arrange(&order, area, 0, 0, None);
        ws.grid_snapshot = Some(snap);
        true
    }

    /// Check the structural invariants of the whole `State` (the 14-point
    /// manifesto contract: every window lives in exactly one place, indices are
    /// in range, focus is valid, geometry is positive/finite, cameras have no
    /// NaN, …). Returns `Ok(())` when clean, or `Err(violations)` listing every
    /// broken invariant.
    ///
    /// This is the single source of truth shared by:
    ///   * `Engine::execute` / `execute_batch` (gated to `debug_assertions` so it
    ///     costs nothing in release), and
    ///   * the property-based harness (Fase 5), which calls it after every step
    ///     regardless of profile.
    ///
    /// It is deliberately cheap and side-effect free — no X11, no cloning of the
    /// client map — so running it on every transition is affordable.
    pub fn check_invariants(&self) -> Result<(), Vec<String>> {
        let mut v = Vec::new();

        // 1. Monitor / workspace indices valid.
        for (mi, mon) in self.monitors.iter().enumerate() {
            if mon.active_ws >= mon.workspaces.len() {
                v.push(format!(
                    "monitor {mi}: active_ws {} out of range ({} workspaces)",
                    mon.active_ws,
                    mon.workspaces.len()
                ));
            }
            // 2. Cameras carry no NaN / infinity (a zeroed spring would desync
            //    the compositor from the logical scroll).
            if !mon
                .workspaces
                .iter()
                .all(|ws| ws.camera.target.is_finite() && ws.camera.position.is_finite())
            {
                v.push(format!("monitor {mi}: camera has NaN/inf target/position"));
            }
            for (ws_i, ws) in mon.workspaces.iter().enumerate() {
                // 3. Focus column/row pointers in range.
                if !ws.columns.is_empty() && ws.focus.column_idx >= ws.columns.len() {
                    v.push(format!(
                        "monitor {mi} ws {ws_i}: focus.column_idx {} >= {} columns",
                        ws.focus.column_idx,
                        ws.columns.len()
                    ));
                }
                for (ci, col) in ws.columns.iter().enumerate() {
                    if !col.windows.is_empty() && col.focused >= col.windows.len() {
                        v.push(format!(
                            "monitor {mi} ws {ws_i} : column {ci}: focused {} >= {} windows",
                            col.focused,
                            col.windows.len()
                        ));
                    }
                }
            }
        }

        // 4. Every window referenced in a tiling/float lives in `clients`, and
        //    no window is referenced more than once across the whole tree.
        let mut seen: std::collections::HashMap<WindowId, (usize, usize)> =
            std::collections::HashMap::new();
        for (mi, mon) in self.monitors.iter().enumerate() {
            for (ws_i, ws) in mon.workspaces.iter().enumerate() {
                for (place, win) in ws
                    .columns
                    .iter()
                    .flat_map(|c| c.windows.iter().copied())
                    .map(|w| ("column", w))
                    .chain(ws.floats.iter().copied().map(|w| ("float", w)))
                {
                    if !self.clients.contains_key(&win) {
                        v.push(format!(
                            "monitor {mi} ws {ws_i}: {place} window {win} not in clients"
                        ));
                        continue;
                    }
                    if let Some(prev) = seen.insert(win, (mi, ws_i)) {
                        v.push(format!(
                            "window {win} referenced twice: at ({}, {}) and ({}, {})",
                            prev.0, prev.1, mi, ws_i
                        ));
                    }
                }
            }
        }

        // 5. Every client *placed in the tree* must be stored at the
        //    (monitor, workspace) its fields claim. A window whose
        //    `monitor`/`workspace` disagree with where it is tiled is a desync
        //    between the logical model and the placement tree. (Orphan clients
        //    that are not yet placed — legitimate in test scaffolding and
        //    during manage — are intentionally NOT required to be in the tree.)
        for (&win, c) in &self.clients {
            if c.monitor >= self.monitors.len() {
                v.push(format!(
                    "client {win}: monitor {} out of range ({})",
                    c.monitor,
                    self.monitors.len()
                ));
                continue;
            }
            let mon = &self.monitors[c.monitor];
            if c.workspace >= mon.workspaces.len() {
                v.push(format!(
                    "client {win}: workspace {} out of range on monitor {} ({} workspaces)",
                    c.workspace,
                    c.monitor,
                    mon.workspaces.len()
                ));
                continue;
            }
            if let Some((pm, pw)) = seen.get(&win).copied() {
                if (pm, pw) != (c.monitor, c.workspace) {
                    v.push(format!(
                        "client {win}: stored at ({}, {}) but tiled at ({}, {})",
                        c.monitor, c.workspace, pm, pw
                    ));
                }
            }
            // NOTE: geometry-positivity and "focused window must be in its active
            // workspace" are deliberately NOT asserted here — they hold only
            // *after* an arrange/placement pass, and many valid intermediate
            // states (and unit-test fixtures) legitimately have a default rect
            // or a transient logical focus. Those are runtime concerns, not
            // structural invariants.
        }

        // 8. Focus stack references only real clients.
        for (mi, mon) in self.monitors.iter().enumerate() {
            for &w in &mon.focus_stack {
                if !self.clients.contains_key(&w) {
                    v.push(format!(
                        "monitor {mi}: focus_stack references unknown window {w}"
                    ));
                }
            }
            // 8b. focus_stack has no duplicates.
            let mut seen_f = std::collections::HashSet::new();
            let mut dup = false;
            for &w in &mon.focus_stack {
                if !seen_f.insert(w) {
                    dup = true;
                }
            }
            if dup {
                v.push(format!("monitor {mi}: focus_stack has duplicate entries"));
            }
            // 8c. The global `pending_focus` slot names a live client and is owned
            // by a currently-presented overlay on the SAME monitor/workspace the
            // deferral was created for. The overlay need not be the *active*
            // workspace — switching workspaces merely hides it from view, it does
            // not dismiss it, so the deferral legitimately survives a workspace
            // switch (it is only consumed/dropped when the overlay is torn down).
            // Requiring the owner to still be the presented overlay is what stops
            // a deferred window from being orphaned: the moment the owner overlay
            // is dismissed or destroyed the deferral is consumed (or dropped).
            if let Some(pf) = self.pending_focus {
                if !self.clients.contains_key(&pf.window) {
                    v.push(format!(
                        "pending_focus window {} is not a known client",
                        pf.window
                    ));
                } else if !self.clients.contains_key(&pf.owner) {
                    v.push(format!(
                        "pending_focus owner {} is not a known client",
                        pf.owner
                    ));
                } else if !self.pending_focus_owner_presented() {
                    v.push(format!(
                        "pending_focus owner {} is not a presented overlay on monitor {} workspace {}",
                        pf.owner, pf.monitor, pf.workspace
                    ));
                }
            }
            // 9b. overlay owner (maximize branch) matches presented_maximize.
            if let Some(w) = self.presented_overlay_owner(mi) {
                if self.clients.get(&w).is_some_and(Client::is_maximized)
                    && mon
                        .workspaces
                        .get(mon.active_ws)
                        .and_then(|ws| ws.presented_maximize)
                        != Some(w)
                {
                    v.push(format!(
                        "monitor {mi}: overlay owner / presented_maximize mismatch"
                    ));
                }
            }
            // NOTE: the logically-focused window is NOT required to be in
            // `focus_stack` here — unit-test fixtures and transient focus
            // retargets legitimately set `mon.focused` before/without updating
            // the stack, and the production focus path keeps them in sync. The
            // dangling-reference check above (stack must name real clients) is
            // the part that catches real corruption.
            // 9. `presented_maximize`, if set, names a real maximized client on the
            //    active workspace.
            let pm = mon
                .workspaces
                .get(mon.active_ws)
                .and_then(|ws| ws.presented_maximize);
            if let Some(w) = pm {
                match self.clients.get(&w) {
                    None => v.push(format!(
                        "monitor {mi}: presented_maximize {w} not in clients"
                    )),
                    Some(c) if !c.is_maximized() => v.push(format!(
                        "monitor {mi}: presented_maximize {w} is not maximized"
                    )),
                    Some(c) if c.workspace != mon.active_ws => v.push(format!(
                        "monitor {mi}: presented_maximize {w} on wrong workspace"
                    )),
                    _ => {}
                }
            }
        }

        // 10. X input focus mirrors a real client.
        if let Some(w) = self.x11_input_focus {
            if !self.clients.contains_key(&w) {
                v.push(format!("x11_input_focus {w} is not a known client"));
            }
        }

        if v.is_empty() {
            Ok(())
        } else {
            Err(v)
        }
    }

    /// `#[cfg(debug_assertions)]` only — panic on the first broken invariant.
    /// Called by `Engine::execute` / `execute_batch` so a bad transition blows up
    /// immediately in debug builds instead of corrupting the model silently.
    #[cfg(debug_assertions)]
    pub fn assert_invariants(&self) {
        if let Err(violations) = self.check_invariants() {
            panic!(
                "State invariant violation:\n  - {}",
                violations.join("\n  - ")
            );
        }
    }

    /// Advance every workspace camera (and per-column boost / zoom springs) by
    /// `dt` seconds. Returns true if any animation is still in flight, so the
    /// backend can keep ticking at a high frame rate.
    pub fn tick_animations(&mut self, dt: f32) -> bool {
        let mut scratch = Vec::new();
        self.tick_animations_multi(dt, &mut scratch)
    }

    /// Like [`State::tick_animations`] but also reports, per monitor, whether that
    /// monitor still has a moving spring (camera, accordion `boost`, or zoom).
    ///
    /// The per-monitor flag lets the frame loop recompute the live layout for
    /// *only* the monitors that are actually animating, instead of (as the global
    /// `bool` did) recomputing every monitor's projection on every animation
    /// frame. An idle monitor whose layout is unchanged keeps its cached
    /// projection (see `WindowManager::run_once`).
    pub fn tick_animations_multi(&mut self, dt: f32, per_monitor: &mut [bool]) -> bool {
        let mut any = false;
        for (mi, mon) in self.monitors.iter_mut().enumerate() {
            let mut anim = false;
            for ws in &mut mon.workspaces {
                if ws.layout == LayoutKind::Column {
                    anim |= ws.camera.step(dt);
                    // Per-column accordion: every column eases its own `boost`
                    // toward 1.0 if it is the focused one, else toward 0.0. This
                    // makes column widths glide when focus changes (bug C10)
                    // instead of snapping. In Overview every boost is forced to 0
                    // so all columns share the base width and the strip fits them.
                    let focus_i = ws.focus.column_idx;
                    for (i, col) in ws.columns.iter_mut().enumerate() {
                        let target = if ws.overview {
                            0.0
                        } else if i == focus_i {
                            1.0
                        } else {
                            0.0
                        };
                        if spring_smooth(&mut col.boost, target, dt) {
                            anim = true;
                        }
                    }
                    if spring_smooth(&mut ws.zoom, ws.zoom_target, dt) {
                        anim = true;
                    }
                    // Viewport page-zoom spring (Fase 9/11): only meaningful in
                    // Zoomed mode, but it is harmless to ease it always — when
                    // the workspace is back to Normal the factor is ignored by
                    // `ribbon_geom` anyway.
                    if spring_smooth(&mut ws.page_zoom, ws.page_zoom_target, dt) {
                        anim = true;
                    }
                }
            }
            if mi < per_monitor.len() {
                per_monitor[mi] = anim;
            }
            any |= anim;
        }
        any
    }
}

/// Critically-damped-ish exponential approach of `cur` toward `target`.
/// Returns true while still moving meaningfully. Stable for any dt.
pub fn spring_smooth(cur: &mut f32, target: f32, dt: f32) -> bool {
    let rate = 12.0;
    let k = 1.0 - (-rate * dt).exp();
    *cur += (target - *cur) * k;
    (*cur - target).abs() > 0.001
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

    #[test]
    fn b4_single_dock_reserves_multiple_edges() {
        // A dock may reserve several edges at once (panel + launcher). All must
        // be applied together, and re-registering must not lose the other edge
        // (bug B4): `set_reserved_region` clears the owner's previous region.
        let mut m = mon();
        m.set_reserved_regions(0x9001, &[(Edge::Top, 22), (Edge::Left, 50)]);
        assert_eq!(m.reserved.top, 22);
        assert_eq!(m.reserved.left, 50);
        assert_eq!(m.reserved_regions.len(), 2);
        // Refreshing with a different edge set replaces the whole owner.
        m.set_reserved_regions(0x9001, &[(Edge::Bottom, 30)]);
        assert_eq!(m.reserved.top, 0);
        assert_eq!(m.reserved.left, 0);
        assert_eq!(m.reserved.bottom, 30);
        assert_eq!(m.reserved_regions.len(), 1);
        // Empty list clears the owner entirely.
        m.set_reserved_regions(0x9001, &[]);
        assert!(m.reserved.is_empty());
        assert!(m.reserved_regions.is_empty());
    }
}

#[cfg(test)]
mod rect_tests {
    use super::Rect;

    #[test]
    fn union_spanning_two_rects_is_the_bounding_box() {
        let a = Rect::new(0, 0, 100, 100);
        let b = Rect::new(300, 200, 50, 50);
        assert_eq!(a.union(b), Rect::new(0, 0, 350, 250));
    }

    #[test]
    fn union_with_overlapping_rect_is_their_bounds() {
        let a = Rect::new(10, 10, 100, 100);
        let b = Rect::new(50, 50, 100, 100);
        assert_eq!(a.union(b), Rect::new(10, 10, 140, 140));
    }

    #[test]
    fn union_with_itself_is_unchanged() {
        let a = Rect::new(5, 5, 40, 40);
        assert_eq!(a.union(a), a);
    }

    #[test]
    fn contains_rect_is_true_only_when_fully_inside() {
        let big = Rect::new(0, 0, 200, 200);
        let small = Rect::new(50, 50, 40, 40);
        assert!(big.contains_rect(small));
        assert!(!small.contains_rect(big));
        // Touching edges counts as contained.
        assert!(big.contains_rect(Rect::new(0, 0, 10, 10)));
        // Partial overlap is not containment.
        assert!(!big.contains_rect(Rect::new(150, 150, 100, 100)));
    }
}
