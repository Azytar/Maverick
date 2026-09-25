//! Columnar layout engine (niri-style) — pure coordinate computation.
//!
//! Owns the `Layout` trait and its registry, `Phase` (`Live` vs `Settled`),
//! `RibbonScratch`/`RibbonGeom`, `FsCtx`, and the `arrange`/`arrange_columns`
//! projection. Geometry is *computed*, never stored: `arrange` is a pure
//! function of `State` + `Cfg` + `Phase` with no I/O, no X11 and no wall-clock,
//! which is what makes the layout testable without a display.
//!
//! `ribbon_geom` is the single geometry source: the arrange loop, the camera
//! target (`ideal_scroll`) and the hit-test extents (`column_screen_extents`)
//! all read the same table, so renderer, camera and hit-test cannot drift.
//!
//! `Phase::Live` reads `camera.position`/`boost`/`zoom` (what the compositor
//! draws this frame); `Phase::Settled` reads `camera.target`/`zoom_target` and
//! the boost targets (where X rests once the animation is over). Both run the
//! identical projection.
//!
//! Not owned here: the presentation overlay (`present::present_into` rewrites
//! placements after layout), the reconciler and `AppliedState`, and backend
//! X11/GL.

use std::collections::HashMap;

use crate::config::Cfg;
use crate::types::{
    Client, LayoutKind, Monitor, Rect, SizeHints, State, ViewportMode, WindowId, Workspace,
};

/// Scratch tuple-vec `(WindowId, Rect, border_w)` that `arrange` fills before
/// `DesiredState::from_placements` makes it explicit. Cleared on every call.
pub type Placements = Vec<(WindowId, Rect, u32)>; // (win, geom, border_w)

/// Which camera/zoom/boost values an `arrange` call should read.
///
/// * `Settled` — the values the layout is *easing toward* (`camera.target`,
///   `zoom_target`, boosted focus column, `page_zoom_target`). The geometry the
///   WM writes to X: the window rests here once the animation is over.
/// * `Live` — the values *this frame* (`camera.position`, `boost`, `zoom`).
///   The compositor draws the same window texture at this position while it
///   glides, so the spring animation is a GPU transform and not a storm of
///   `ConfigureWindow`s.
///
/// The two paths share every bit of projection math except this one choice, so
/// they can never drift apart. This is the `Phase::Live`/`Phase::Settled`
/// split that `arrange` takes as a parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Settled,
    Live,
}

impl Phase {
    fn is_live(self) -> bool {
        matches!(self, Phase::Live)
    }
    fn is_settled(self) -> bool {
        matches!(self, Phase::Settled)
    }
}

// A `Layout` is a pluggable arrangement strategy. The core never matches on
// `LayoutKind` — it asks the registry for the strategy's `arrange()` — so
// arrangement is the one concern a new layout can change on its own.

pub trait Layout: Send + Sync {
    fn name(&self) -> &'static str;
    fn arrange(
        &self,
        state: &State,
        mon: &Monitor,
        cfg: &Cfg,
        phase: Phase,
        out: &mut Placements,
        scratch: &mut RibbonScratch,
    );
}

/// Reusable scratch for the per-frame column projection.
///
/// `ribbon_geom` builds a per-column `(x, width)` table that the arrange loop
/// then reads back by column index. Building that table allocates a `Vec` every
/// call, and `arrange` runs once per animating monitor per frame — so the table
/// is owned here and reused. Boxed so the trait object stays thin and the
/// scratch can be passed through `&dyn Layout` without sizing it into every
/// caller.
pub struct RibbonScratch {
    cols: Vec<(f32, f32)>,
}

impl Default for RibbonScratch {
    fn default() -> Self {
        Self {
            cols: Vec::with_capacity(32),
        }
    }
}

impl RibbonScratch {
    /// Hand the underlying buffer to `ribbon_geom_into`, keeping the lifetime
    /// simple: the geometry is returned by value, the columns stay alive here.
    pub(crate) fn ribbon_geom(
        &mut self,
        ws: &Workspace,
        cfg: &Cfg,
        workarea: Rect,
        settled: bool,
        fs: &FsCtx,
    ) -> RibbonGeom<'_> {
        ribbon_geom_into(ws, cfg, workarea, settled, fs, &mut self.cols)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ColumnLayout;

impl Layout for ColumnLayout {
    fn name(&self) -> &'static str {
        "column"
    }
    fn arrange(
        &self,
        state: &State,
        mon: &Monitor,
        cfg: &Cfg,
        phase: Phase,
        out: &mut Placements,
        scratch: &mut RibbonScratch,
    ) {
        arrange_columns(state, mon, cfg, phase, out, scratch);
    }
}

// Maps `LayoutKind` → `Box<dyn Layout>`. The backend builds one instance and
// shares it with every arrange caller.

pub struct LayoutRegistry {
    layouts: HashMap<LayoutKind, Box<dyn Layout>>,
}

impl LayoutRegistry {
    pub fn new() -> Self {
        let mut r = Self {
            layouts: HashMap::new(),
        };
        r.register(LayoutKind::Column, Box::new(ColumnLayout));
        r
    }

    pub fn register(&mut self, kind: LayoutKind, layout: Box<dyn Layout>) {
        self.layouts.insert(kind, layout);
    }

    pub fn get(&self, kind: LayoutKind) -> &dyn Layout {
        match self.layouts.get(&kind) {
            Some(layout) => layout.as_ref(),
            // Fallback to Column if an unknown layout is somehow selected
            None => self.layouts.get(&LayoutKind::Column).unwrap().as_ref(),
        }
    }

    pub fn all_kinds(&self) -> Vec<LayoutKind> {
        self.layouts.keys().copied().collect()
    }
}

impl Default for LayoutRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// `arrange` knows only logical layout geometry (the layout rect). The
// *maximized* presentation overlay is applied afterwards by
// `core::present::present_into`. Fullscreen is not a pinned always-on-top
// overlay: a non-exclusive fullscreen window becomes one column of the scrolling
// ribbon whose single tile measures `mon.screen`, so it scrolls with the camera
// and leaves the screen when focus moves to a neighbour. This is driven entirely
// by `FsCtx` — a derived descriptor (never stored) passed through
// `ribbon_geom`, `arrange_columns`, `ideal_scroll` and `column_screen_extents`
// so all four agree on where the fullscreen column sits.

/// Count the number of tiled (non-floating) windows on a workspace.
fn count_tiled(ws: &Workspace) -> usize {
    ws.columns.iter().map(|c| c.windows.len()).sum()
}

/// A derived descriptor of the fullscreen columns on a workspace, if any. It
/// is NEVER stored in `State` — it is recomputed at every call site that needs
/// ribbon geometry, so `ribbon_geom`, `ideal_scroll` and
/// `column_screen_extents` can share one consistent view of where the
/// fullscreen tiles live without violating the borrow checker (those helpers
/// receive `&Workspace`, not `&State`).
///
/// The fullscreen window of a column is the FIRST window in that column with
/// the `FULLSCREEN` flag. Several columns may be fullscreen at once (niri-style:
/// each is a screen-filling ribbon column you scroll between with h/l). In a
/// non-`Column` layout (no scroll ribbon to join) and in Overview (the
/// fullscreen tile is shown scaled, like any other tile) `cols`/`wins` are
/// empty and the windows fall back to their normal tile slots.
///
/// `win` is the *focused* fullscreen window (the fullscreen window of the
/// focused column, if that column is itself fullscreen) — used by the renderer's
/// "covering" stacking rule. It is `None` when focus is not on a fullscreen
/// column, so only the column you are actually looking at is raised above the
/// dock.
#[derive(Debug, Clone, Default)]
pub struct FsCtx {
    /// Indices of every column hosting a fullscreen (non-`True`) window.
    pub cols: Vec<usize>,
    /// The fullscreen window of each such column, parallel to `cols`.
    pub wins: Vec<WindowId>,
    /// The focused fullscreen window, if the focused column is a fullscreen one.
    pub win: Option<WindowId>,
    /// The full-screen box (`mon.screen`) the tiles should fill.
    pub screen: Rect,
}

/// Pure derivation of `FsCtx`. Returns empty `cols`/`wins` when the workspace is
/// not a `Column` layout or is in Overview (the fullscreen tiles are then just
/// normal, scaled, ribbon participants and never overlays).
///
/// Windows with `FullscreenPolicy::True` are excluded here — and *only* here,
/// so `ribbon_geom`, `ideal_scroll` and `column_screen_extents` can never
/// disagree about where the ribbon's fullscreen columns sit. Their fullscreen
/// is an exclusive overlay outside the ribbon (`core::present`), so as far as
/// the ribbon is concerned they are still in their ordinary tile.
pub fn fs_ctx(
    clients: &HashMap<WindowId, Client, impl std::hash::BuildHasher>,
    ws: &Workspace,
    screen: Rect,
) -> FsCtx {
    if ws.layout != LayoutKind::Column || ws.overview {
        return FsCtx::default();
    }
    let mut cols: Vec<usize> = Vec::new();
    let mut wins: Vec<WindowId> = Vec::new();
    for (ci, col) in ws.columns.iter().enumerate() {
        if let Some(&w) = col.windows.iter().find(|&&w| {
            clients
                .get(&w)
                .is_some_and(|c| c.is_fullscreen() && !c.is_true_fullscreen())
        }) {
            cols.push(ci);
            wins.push(w);
        }
    }
    // The focused fullscreen window: the fullscreen window of the focused
    // column, but only when that column is itself a fullscreen column.
    let win = ws
        .columns
        .get(ws.focus.column_idx)
        .and_then(|col| {
            col.windows.iter().find(|&&w| {
                clients
                    .get(&w)
                    .is_some_and(|c| c.is_fullscreen() && !c.is_true_fullscreen())
            })
        })
        .copied()
        .filter(|_| cols.contains(&ws.focus.column_idx));
    FsCtx {
        cols,
        wins,
        win,
        screen,
    }
}

/// Ceiling for user-configured gaps at the u32→i32 boundary
/// (see `effective_gaps`). Any value above this is already "all gap" on any
/// real display; the ordering of user intent below it is preserved.
pub(crate) const MAX_CFG_GAP: i32 = 1_000_000;

/// Resolve the effective inner/outer gaps for this workspace, applying
/// `smart_gaps` (collapse to 0 when exactly one tiled window).
///
/// This is also the u32→i32 representation boundary for user config: a raw
/// `gaps_outer` above `i32::MAX` would wrap negative here and drive the
/// workarea in the *wrong* direction. Clamping to [`MAX_CFG_GAP`] is not a
/// behavior change — beyond ~1M px a gap already exceeds any display — but it
/// keeps the numbers sane so the workarea-aware gap reduction in
/// `arrange_columns` can do its real job: preserve the user's intent (big gaps)
/// while guaranteeing valid geometry.
fn effective_gaps(ws: &Workspace, cfg: &Cfg) -> (i32, i32) {
    if cfg.smart_gaps && count_tiled(ws) <= 1 && ws.floats.is_empty() {
        return (0, 0);
    }
    (
        cfg.gaps_inner.min(MAX_CFG_GAP as u32) as i32,
        cfg.gaps_outer.min(MAX_CFG_GAP as u32) as i32,
    )
}

/// Project `mon_idx`'s active workspace into `out`. Idempotent by contract: the
/// buffer is cleared and refilled, never appended to and never reallocated, so a
/// caller may run this once per monitor per frame over a reused buffer.
///
/// `phase` selects whether the window rests at its settled (target) geometry or
/// is drawn at the live (current) geometry — see [`Phase`].
pub fn arrange(
    state: &State,
    mon_idx: usize,
    cfg: &Cfg,
    registry: &LayoutRegistry,
    phase: Phase,
    out: &mut Placements,
    scratch: &mut RibbonScratch,
) {
    let Some(mon) = state.monitors.get(mon_idx) else {
        // Stale monitor index after hotplug: produce no placements instead
        // of panicking; the reconciler keeps the last applied frame.
        out.clear();
        return;
    };
    let Some(ws) = mon.workspaces.get(mon.active_ws) else {
        out.clear();
        return;
    };
    let layout = registry.get(ws.layout);
    // `out` is the WM's *shared* `desired` buffer, which the compositor
    // animation path also writes into (`compositor::live_placements`). Without
    // this clear the previous frame's live placements leak in here, get
    // re-applied by `apply_geom`, and physically re-show windows that
    // `hide_offscreen` just moved off-screen — a fullscreen window on the
    // previously active workspace would reappear covering the current one.
    out.clear();
    layout.arrange(state, mon, cfg, phase, out, scratch);
}

// Each column sits at a fixed x position (derived from the sum of prior column
// widths + gaps). Windows within a column split vertically into uniformly-sized
// rows: focus never changes a window's geometry (no reflow on Up/Down
// navigation), it is marked with border/color only.

/// The column-ribbon geometry every consumer derives its numbers from. The
/// renderer (`arrange_columns`), the camera target (`ideal_scroll`) and
/// `column_screen_extents` all read this one table, so they cannot drift.
pub(crate) struct RibbonGeom<'a> {
    /// Workarea inset by `gaps_outer` on all four edges.
    pub wa: Rect,
    /// Semantic-zoom factor applied, clamped to >= 0.05.
    pub alpha: f32,
    /// `wa.w * (1 - alpha) / 2` — horizontal zoom-around offset.
    pub cx: f32,
    /// `wa.h * (1 - alpha) / 2` — vertical zoom-around offset.
    pub cy: f32,
    /// Effective inner gap (after `smart_gaps`).
    pub gap: f32,
    /// `(world_x, world_w)` of each column, including the accordion boost.
    /// Borrowed from the caller's `RibbonScratch` so the per-frame path
    /// allocates neither this table nor the Vec behind it.
    pub cols: &'a [(f32, f32)],
    /// Total ribbon width in world px (0 if no columns).
    pub total_w: f32,
}

/// Owned sibling of [`RibbonGeom`] (columns held by value, not borrowed). Only
/// the convenience `ribbon_geom` wrapper produces it; the per-frame path uses
/// the borrowed form so no `Vec` is ever allocated.
pub(crate) struct RibbonGeomOwned {
    pub wa: Rect,
    pub alpha: f32,
    pub cx: f32,
    pub cols: Vec<(f32, f32)>,
    pub total_w: f32,
}

/// `settled = true` uses the rest (animated) values of the per-column boost and
/// of `zoom` (`ws.zoom_target`) so the camera can target where the layout *will*
/// land. `settled = false` uses the live values so this matches what is
/// actually on screen this frame.
///
/// Convenience wrapper: builds its own scratch. Callers on the per-frame path
/// should use [`RibbonScratch::ribbon_geom`] with a reused buffer instead.
pub(crate) fn ribbon_geom(
    ws: &Workspace,
    cfg: &Cfg,
    workarea: Rect,
    settled: bool,
    fs: &FsCtx,
) -> RibbonGeomOwned {
    let mut scratch = RibbonScratch::default();
    let g = ribbon_geom_into(ws, cfg, workarea, settled, fs, &mut scratch.cols);
    RibbonGeomOwned {
        wa: g.wa,
        alpha: g.alpha,
        cx: g.cx,
        cols: g.cols.to_vec(),
        total_w: g.total_w,
    }
}

/// Like [`ribbon_geom`] but writes the per-column table into `cols` (a reused
/// buffer supplied by the caller) and borrows it back, so the per-frame
/// projection allocates nothing. `cols` is cleared first, so a buffer grown to
/// the column count on one monitor is reused (never re-grown) on every other.
pub(crate) fn ribbon_geom_into<'s>(
    ws: &Workspace,
    cfg: &Cfg,
    workarea: Rect,
    settled: bool,
    fs: &FsCtx,
    cols: &'s mut Vec<(f32, f32)>,
) -> RibbonGeom<'s> {
    let (gap, gap_outer) = effective_gaps(ws, cfg);
    // The outer gap insets the workarea on all four edges — unlike the
    // scrollable horizontal ribbon, it anchors real geometry. A gap larger
    // than half the workarea pushes the inset area off-monitor (saturating to a
    // zero-area workarea *anchored outside the screen*, e.g. a window at y=5000
    // on a 1080 px display), so clamp to the largest gap that keeps the inset
    // inside the workarea: a "huge gap" still yields the maximum possible
    // inset and the geometry stays valid.
    let gap_outer = gap_outer
        .min(workarea.w as i32 / 2)
        .min(workarea.h as i32 / 2)
        .max(0);
    let wa = Rect::new(
        workarea.x + gap_outer,
        workarea.y + gap_outer,
        workarea.w.saturating_sub((2 * gap_outer) as u32),
        workarea.h.saturating_sub((2 * gap_outer) as u32),
    );

    let alpha = (if settled { ws.zoom_target } else { ws.zoom }).max(0.05);
    // Viewport zoom: when the workspace is in `Zoomed` mode the zoom factor is
    // `page_zoom` (which may be > 1 to *enlarge* the ribbon), not the Overview
    // `zoom`. They are kept separate on purpose — Overview zooms out
    // (`alpha < 1`), Viewport zooms in (`alpha > 1`). `ribbon_geom` has no
    // upper clamp on `alpha`, so the enlargement falls out for free.
    let alpha = if ws.viewport_mode == ViewportMode::Zoomed {
        if settled {
            ws.page_zoom_target
        } else {
            ws.page_zoom
        }
        .max(0.05)
    } else {
        alpha
    };
    let cx = (wa.w as f32 * (1.0 - alpha)) / 2.0;
    let cy = (wa.h as f32 * (1.0 - alpha)) / 2.0;
    let gap_f = gap as f32;
    // Each column's width is a fraction of the FULL workarea width, *independent
    // of how many columns exist*: adding a column must not shrink the others.
    // The ribbon simply grows and the camera scrolls (niri-style).
    let usable_w = wa.w as f32;

    // Per-column accordion boost: the focused column eases toward 1.0 and the
    // others toward 0.0 (see `Workspace::tick_animations`), so changing focus
    // makes the widths *glide* instead of snapping. In Overview the boost is
    // forced to 0 so every column sits at its base width and the strip fits all
    // of them.
    let total_boost = cfg.accordion_boost.clamp(0.0, 0.9);
    let focus_i = ws.focus.column_idx;

    cols.clear();
    let mut x: f32 = 0.0;
    for (i, c) in ws.columns.iter().enumerate() {
        let boost = if ws.overview {
            0.0
        } else if settled {
            // The settled boost is the *target* of the animation: the focused
            // column rests at 1.0 and every other column at 0.0. The camera
            // eases toward this projection, so it needs a fixed point; reading
            // the live boost here would make the target track the animation it
            // drives, which reads as residual slowness.
            if i == focus_i {
                1.0
            } else {
                0.0
            }
        } else {
            c.boost
        };
        // A fullscreen column in the scrolling ribbon is exactly `mon.screen`
        // wide — already at maximum width — so the accordion boost does not
        // apply and its world width is independent of the workarea width.
        let w = if fs.cols.contains(&i) {
            fs.screen.w as f32
        } else {
            let boosted = (c.weight + total_boost * boost).min(1.0);
            boosted * usable_w
        };
        cols.push((x, w));
        x += w + gap_f;
    }
    let total_w = (x - gap_f).max(0.0);

    RibbonGeom {
        wa,
        alpha,
        cx,
        cy,
        gap: gap_f,
        cols: &cols[..],
        total_w,
    }
}

/// Ceiling for the user-configured border width at the u32→i32 boundary
/// (see [`effective_border_w`]). Same rationale as [`MAX_CFG_GAP`]: any value
/// above this already covers any real display, and `2 * MAX_CFG_BORDER` is
/// `2_000_000`, so the frame reserved on both sides of a row is far inside
/// `i32`.
pub(crate) const MAX_CFG_BORDER: i32 = 1_000_000;

/// The border width the projection may reserve around a tiled window, resolved
/// from a `Cfg::border_w` the caller is free to set without validating it (the
/// config file and the IPC `SetBorderWidth` command both do).
///
/// This is the u32→i32 representation boundary for user config: a raw
/// `border_w` above `i32::MAX` wraps negative here and would *add* the frame to
/// a row instead of reserving it, and one above `i32::MAX / 2` overflows the
/// `2 * bw` the frame costs. Clamping to [`MAX_CFG_BORDER`] is not a behavior
/// change — past ~1M px a border already exceeds any display, where the `.max(1)`
/// protocol floor already dominates — but it keeps the frame arithmetic exact
/// and every emitted rectangle a valid one. The clamped value is also the one
/// reported in [`Placements`], so a window is always configured with the border
/// its geometry was computed from.
fn effective_border_w(cfg: &Cfg) -> u32 {
    cfg.border_w.min(MAX_CFG_BORDER as u32)
}

fn arrange_columns(
    state: &State,
    mon: &Monitor,
    cfg: &Cfg,
    phase: Phase,
    out: &mut Placements,
    scratch: &mut RibbonScratch,
) {
    let ws = mon.ws();
    let full_wa = mon.workarea;
    let bw = effective_border_w(cfg);

    // Derived fullscreen descriptor — the single source of truth for where the
    // fullscreen tile lives in the ribbon. It is computed here (not stored) so
    // `ribbon_geom` and the camera target can share it without a `&State` borrow.
    let fs = fs_ctx(&state.clients, ws, mon.screen);

    // Single source of truth: the ribbon geometry for the requested phase.
    // `ribbon_geom_into` takes `settled` (targets) — pass `is_settled()`, NOT
    // `is_live()`: those are independent animations, and inverting this one
    // inverts both (one-shot arranges read mid-flight springs, while live
    // frames jump straight to targets and every glide snaps).
    let g = scratch.ribbon_geom(ws, cfg, full_wa, phase.is_settled(), &fs);
    let wa = g.wa;

    // `Phase::Settled` projects to the camera's *rest* position (`target`) so a
    // one-shot `arrange` (the compositor path, which does not reconfigure X
    // every frame) leaves X windows at the final, correct spot — matching the
    // compositor's drawn position at rest. `Phase::Live` projects to the live
    // `position` so the X11-only path animates smoothly each frame.
    let cam = if phase.is_live() {
        ws.camera.position
    } else {
        ws.camera.target
    };

    for (col_idx, col) in ws.columns.iter().enumerate() {
        // A fullscreen column in the scrolling ribbon is a single screen-filling
        // tile that scrolls with the camera. Emit one placement for its window
        // and hide the column's siblings while the fullscreen is active.
        if fs.cols.contains(&col_idx) {
            let (world_x, _col_w_world) = g.cols[col_idx];
            if let Some(win) = col
                .windows
                .iter()
                .find(|&&w| {
                    state
                        .clients
                        .get(&w)
                        .is_some_and(|c| c.is_fullscreen() && !c.is_true_fullscreen())
                })
                .copied()
            {
                let screen = fs.screen;
                let alpha = g.alpha;
                let cx = g.cx;
                let screen_col_x = (wa.x as f32 + (world_x - cam) * alpha + cx).round() as i32;
                // Scale the full-screen box around its own vertical centre so
                // that at `alpha == 1` it is exactly `mon.screen`.
                let screen_y =
                    (screen.y as f32 + screen.h as f32 * (1.0 - alpha) / 2.0).round() as i32;
                let screen_w = (screen.w as f32 * alpha).max(1.0) as u32;
                let screen_h = (screen.h as f32 * alpha).max(1.0) as u32;
                if state.clients.contains_key(&win) {
                    out.push((
                        win,
                        Rect::new(screen_col_x, screen_y, screen_w, screen_h),
                        0,
                    ));
                }
            }
            continue;
        }
        let (world_x, col_w_world) = g.cols[col_idx];
        let n = col.windows.len();
        if n == 0 {
            continue;
        }

        let alpha = g.alpha;
        let cx = g.cx;
        let cy = g.cy;
        let gap_f = g.gap;

        // In X11, ConfigureWindow's x/y already mark the outer (border-
        // inclusive) top-left corner, and width/height are content-only —
        // so bw is subtracted from the content width here. The `.max(1.0)`
        // floor is the protocol minimum: a `ConfigureWindow` with width or
        // height 0 is `BadValue` and the server silently drops the request,
        // leaving `Applied` ahead of reality.
        let inner_w = ((col_w_world * alpha) - 2.0 * bw as f32).max(1.0) as u32;
        // Only the (n-1) gaps *between* rows are reserved; top/bottom edges
        // sit flush with `wa`. Vertical also scales by `alpha` in Overview.
        // Clamp the vertical gap so the windows always have at least 1px of
        // vertical space, preventing `total_h` from going negative and
        // pushing windows entirely out of the workarea.
        let max_total_gaps = (wa.h as f32 - n as f32).max(0.0);
        let max_gap = if n > 1 {
            max_total_gaps / (n as f32 - 1.0)
        } else {
            0.0
        };
        let gap_f_v = gap_f.min(max_gap);
        let total_h = wa.h as f32 - (n as f32 - 1.0) * gap_f_v;

        // Uniform rows: the last row absorbs any remainder so the column always
        // fills `total_h` exactly; focus never resizes rows. Computed inline
        // per row rather than collected into a `Vec`, so the per-frame
        // projection allocates nothing.
        let base_h = if n > 1 { total_h / n as f32 } else { total_h };
        let extra_last = if n > 1 {
            total_h - base_h * n as f32
        } else {
            0.0
        };

        // Map world coords (workarea px, pre-camera, pre-zoom) into screen
        // coords: scale by `alpha` around the workarea center (cx/cy), then
        // subtract the camera scroll. At alpha = 1 this is exactly the
        // original niri-style mapping.
        //
        // Round here, at the integer X11 boundary, and nowhere earlier: world
        // coords stay fractional through layout so a fractional camera offset
        // does not accumulate per-column rounding error across the gap
        // sequence and make adjacent columns drift apart mid-scroll.
        let screen_col_x = (wa.x as f32 + (world_x - cam) * alpha + cx).round() as i32;

        for (ri, &win) in col.windows.iter().enumerate() {
            if !state.clients.contains_key(&win) {
                continue;
            }

            let extra = if n > 1 && ri == n - 1 {
                extra_last
            } else {
                0.0
            };
            let row_h_world = (base_h + extra).max(1.0);
            let row_y_world = wa.y as f32 + ri as f32 * (base_h + gap_f_v);
            let screen_h = (row_h_world * alpha).max(1.0) as u32;
            let screen_y = (wa.y as f32 + (row_y_world - wa.y as f32) * alpha + cy).round() as i32;

            let geom = Rect::new(
                screen_col_x,
                screen_y,
                inner_w,
                (screen_h as i32 - 2 * bw as i32).max(1) as u32,
            );
            out.push((win, geom, bw));
        }
    }

    // Floating windows keep their existing geometry, normalized to the
    // workarea.
    //
    // Single authority for floats (see `normalize_float_geom`): arrange is a
    // pure projection — it never mutates `client.geom` — and normalizes with
    // the same idempotent function that drag / `ConfigureRequest` /
    // `MoveResize` use. A float that has not moved therefore has a `Desired`
    // bit-for-bit equal to its `Applied`, and the reconciler emits no spurious
    // `ConfigureWindow`.
    //
    // Exception — `float_client_authority`: the WM adopted the client's rect
    // verbatim (it is the `ConfigureRequest` sink). Re-normalizing it here
    // would rewrite what was promised and reopen the ping-pong (client re-asks
    // → WM re-writes: the "float that jumps on its own"). While the seal
    // lives, the projection is the adopted rect with only protocol-level
    // sanity applied. The seal is cleared when the WM decides again (drag,
    // rules, `ToggleFloat`, workarea/monitor change — `settle_float_in_workarea`).
    for &win in &ws.floats {
        let client = match state.clients.get(&win) {
            Some(c) => c,
            None => continue,
        };
        let bw = client.border_w;
        let g = if client.float_client_authority {
            adopt_client_float_geometry(client.geom)
        } else {
            normalize_float_geom(client.geom, client.hints, full_wa, bw)
        };
        // The client's own border width, so a `Rule::border_w` override takes
        // effect for floating windows.
        out.push((win, g, bw));
    }
}

// Camera/hit-test helpers, all reading the same `ribbon_geom` table.

/// Horizontal extents (in SCREEN space) of each column, using the exact same
/// projection as `arrange_columns`. Used by the Mod4+wheel camera step to know
/// where each column actually sits on screen — it must match what is drawn,
/// not a stale world-space estimate.
pub(crate) fn column_screen_extents(
    ws: &Workspace,
    cfg: &Cfg,
    workarea: Rect,
    fs: &FsCtx,
) -> Vec<(f32, f32)> {
    let mut out = Vec::new();
    let mut scratch = RibbonScratch::default();
    column_screen_extents_into(ws, cfg, workarea, fs, &mut out, &mut scratch);
    out
}

/// Allocation-free variant used by the compositor's visual path.
pub(crate) fn column_screen_extents_into(
    ws: &Workspace,
    cfg: &Cfg,
    workarea: Rect,
    fs: &FsCtx,
    out: &mut Vec<(f32, f32)>,
    scratch: &mut RibbonScratch,
) {
    let g = ribbon_geom_into(ws, cfg, workarea, false, fs, &mut scratch.cols);
    out.clear();
    out.extend(g.cols.iter().enumerate().map(|(i, &(x, w))| {
        // Match `arrange_columns`' geometry exactly: the right edge is the
        // *inner* (border-exclusive) width, so the hit-test extent agrees
        // with the `client.geom` X11 hit-tests against (invariant A). A
        // fullscreen column is drawn with border 0, so it contributes no
        // border to subtract; tiled columns reserve [`effective_border_w`].
        let bw = if fs.cols.contains(&i) {
            0.0
        } else {
            effective_border_w(cfg) as f32
        };
        let l = g.wa.x as f32 + (x - ws.camera.position) * g.alpha + g.cx;
        let inner_w = (w * g.alpha - 2.0 * bw).max(1.0);
        (l, l + inner_w)
    }));
}

/// Compute the ideal scroll so the focused column is fully visible (niri-style
/// centering). Takes the explicit workspace and its real `workarea`, so it
/// always targets the workspace the caller intends (not `mon.ws()`, which may
/// be a different, active one). Returns a settled target: it reads the rest
/// values of the animated factors (`zoom_target`, accordion as a step) so the
/// spring eases to a fixed point and overshoots cleanly.
pub fn ideal_scroll(ws: &Workspace, cfg: &Cfg, workarea: Rect, fs: FsCtx) -> f32 {
    let g = ribbon_geom(ws, cfg, workarea, true, &fs);
    if g.cols.is_empty() {
        return 0.0;
    }
    let i = ws.focus.column_idx.min(g.cols.len() - 1);
    let (x, w) = g.cols[i];
    let waw = g.wa.w as f32;

    let cam_min = g.cx / g.alpha;
    let cam_max = g.total_w - (waw - g.cx) / g.alpha;

    if fs.cols.contains(&i) && ws.layout == LayoutKind::Column {
        // The focused column is the fullscreen one: align its left edge exactly
        // to `screen.x` instead of centering it in the workarea. Centering would
        // leave a residual offset of `strut_left/2` with asymmetric struts (a
        // side dock), because the column is `screen.w` wide (taller than the
        // workarea) and lives in world space measured from `wa.x`, not `screen.x`.
        let cam = x + (g.wa.x as f32 + g.cx - fs.screen.x as f32) / g.alpha;
        if cam_max <= cam_min {
            (g.total_w - waw) / 2.0
        } else {
            cam.clamp(cam_min, cam_max)
        }
    } else {
        let want = x + w / 2.0 - waw / 2.0;

        // Zoom-aware clamp: the left screen edge maps to world `cam - cx/alpha`,
        // and the visible world span is `waw/alpha`. When the whole ribbon fits
        // inside that span, center it (also fixes Overview for free).
        if cam_max <= cam_min {
            (g.total_w - waw) / 2.0
        } else {
            want.clamp(cam_min, cam_max)
        }
    }
}

/// Normalize a floating rect **the WM decided** (initial placement, rules,
/// drag/resize, `ToggleFloat`, monitor change) against `SizeHints` +
/// workarea, idempotently (`f(f(x)) == f(x)`).
///
/// Canonical order: `snap_float_to_hints` -> `clamp_float_geom` ->
/// `settle_to_grid`. The middle clamp guarantees the grid can never push the
/// rect outside the workarea, and the final settle only shrinks within
/// `[min, clamped]`, so the answer lands on the grid the client itself declares
/// and there is nothing left for it to correct (a float that does not move
/// generates no spurious `ConfigureWindow`).
///
/// # Authority: never apply this to a rect the client asked for
///
/// This function *rewrites* the rect it is given. That is correct when the rect
/// is a WM choice (nobody else will claim it) and is exactly the error that
/// produces the ping-pong when applied to a client request: the client asks
/// again and the WM rewrites again, forever. For a rect arriving from
/// `ConfigureRequest` (the client is the authority) use
/// [`adopt_client_float_geometry`].
///
/// Pure over `(Rect, SizeHints, Rect, u32)`: no X11, no `&State`.
pub fn normalize_float_geom(g: Rect, hints: SizeHints, wa: Rect, border_w: u32) -> Rect {
    settle_to_grid(
        clamp_float_geom(snap_float_to_hints(g, hints), wa, border_w),
        hints,
    )
}

/// Re-settle a float as a fixed point of the WM's normalization against the
/// workarea of `mi` — the single helper for "the float acquired a new context":
/// `ToggleFloat`, workspace/monitor change, de-promotion from fullscreen, a
/// `WM_NORMAL_HINTS` refresh.
///
/// Why it exists: a float inserted into a workarea other than the one that
/// shaped its rect arrives with a `geom` that is NOT a fixed point of the
/// projection (the new context's hint grid, the new workarea clamp). Without
/// re-settling it, the first `arrange` corrects it — a visible jump *after* the
/// change — and if the client then claims its rect, the ping-pong returns.
/// Normalizing once here, at the moment the context is acquired, leaves the
/// first arrange nothing to correct: one configure, zero jumps.
///
/// Also clears the `float_client_authority` seal: the WM is deciding again (the
/// rect it settles is a WM choice, already on the client's grid). Idempotent by
/// construction, since `normalize_float_geom` is.
///
/// Pure over `&mut State`: no X11, no `&mut Cfg`.
pub fn settle_float_in_workarea(state: &mut State, mi: usize, win: WindowId) {
    let Some(c) = state.clients.get_mut(&win) else {
        return;
    };
    if !c.is_float() {
        return;
    }
    let Some(mon) = state.monitors.get(mi) else {
        return;
    };
    let (geom, hints, bw, wa) = (c.geom, c.hints, c.border_w, mon.workarea);
    let settled = normalize_float_geom(geom, hints, wa, bw);
    let Some(c) = state.clients.get_mut(&win) else {
        return;
    };
    c.geom = settled;
    c.saved_geom = settled;
    c.float_client_authority = false;
}

/// Adopt the floating rect **the client asked for**: the client is the
/// authority and the WM only keeps what X can no longer represent.
///
/// This is the *compliant* half of the float policy, and the reason a float is
/// stable: `f(x) == x` for every representable rect, so the client's
/// correction function (the `ConfigureRequest`/`XResizeWindow` a toolkit
/// re-sends when it believes its geometry was not honoured) has its fixed
/// point on the *first* request. A WM that rewrites the request (snaps to the
/// grid, clamps to the workarea) creates the loop: client asks for A, WM answers
/// B, client asks for A again … — the float "dances" and the WM burns CPU.
///
/// Only the values the protocol cannot express are sanitized:
/// * `w`/`h` into `1..=u16::MAX` (a `ConfigureWindow` with 0 is `BadValue`, and
///   the server drops the request, leaving `Applied` ahead of reality);
/// * `x`/`y` into the `i16` range that `ConfigureNotify` transports.
///
/// No snap to hints, no workarea clamp, no settle: none of those can improve a
/// rect the client chose, and any of them can make it worse. The *position* may
/// legitimately land partly outside the workarea (a strut-inset workarea is
/// invisible to a client that centres itself on the screen); the WM guarantees
/// reachability instead, by placing new floats inside
/// (`normalize_float_geom`) and by reclaiming the full workarea when it changes
/// (`reposition_floats`).
///
/// Pure over `Rect`: no X11, no `&State`.
pub fn adopt_client_float_geometry(g: Rect) -> Rect {
    let clamp_i16 = |v: i32| v.clamp(i32::from(i16::MIN), i32::from(i16::MAX));
    Rect::new(
        clamp_i16(g.x),
        clamp_i16(g.y),
        g.w.clamp(1, u32::from(u16::MAX)),
        g.h.clamp(1, u32::from(u16::MAX)),
    )
}

/// Clip a floating rect to the workarea, reserving the `2 * border_w` frame.
///
/// Size before position, idempotent, never degenerate (`w`/`h >= 1`). The
/// backend re-exports it as a thin adapter (`render::clamp_float_to_workarea`)
/// so the float policy is not forked in two places.
///
/// Pure over `(Rect, Rect, u32)`: no X11, no `&State`.
pub fn clamp_float_geom(mut g: Rect, wa: Rect, border_w: u32) -> Rect {
    let frame = i64::from(border_w) * 2;
    let max_w = (i64::from(wa.w) - frame).clamp(1, i64::from(u16::MAX)) as u32;
    let max_h = (i64::from(wa.h) - frame).clamp(1, i64::from(u16::MAX)) as u32;
    g.w = g.w.clamp(1, max_w);
    g.h = g.h.clamp(1, max_h);
    let max_x = (i64::from(wa.x) + i64::from(wa.w) - i64::from(g.w) - frame).max(i64::from(wa.x));
    let max_y = (i64::from(wa.y) + i64::from(wa.h) - i64::from(g.h) - frame).max(i64::from(wa.y));
    g.x = g.x.clamp(
        wa.x,
        max_x.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
    );
    g.y = g.y.clamp(
        wa.y,
        max_y.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
    );
    g
}

/// Snap a floating window's requested size to its `WM_NORMAL_HINTS`
/// (minimum / maximum / base / increment). Pure over `(Rect, SizeHints)` —
/// no X11, no `&self` — so the drag-resize path and the client
/// `ConfigureRequest` path share one interpretation and can never disagree.
///
/// Why this exists: a hint-respecting toolkit (Qt, Xt, …) corrects any size
/// that violates its own hints with an immediate follow-up `ConfigureRequest`.
/// Answering a client resize with a hint-violating size therefore guarantees a
/// corrective bounce on *every* update — for a progress/download dialog that
/// resizes itself dozens of times per second (e.g. `PrismLauncher`'s resource
/// download window) the window visibly jumps bigger/smaller on each bounce.
/// Snapping first makes the WM's answer something the toolkit accepts, so a
/// resize storm terminates in exactly one configure per distinct size.
///
/// Order matters: min/max clamp, increment round-snap (nearest multiple of
/// `(size - base)`, the rounding the drag path uses), then min/max clamp again
/// as the final word. Hard `[min, max]` bounds win over
/// the increment grid because a toolkit always accepts its own min/max, so
/// the result is a fixed point of the client's correction function even when
/// the hints themselves are not increment-aligned (pathological).
///
/// `!valid` (or all-zero) hints mean "no constraint" and return `g`
/// unchanged, so clients without hints keep pass-through semantics. Position
/// is never touched — only `w`/`h`.
///
/// Note: the result may still need a workarea clamp afterwards (see
/// `normalize_float_request` in `backend::x11::render`): the screen is a
/// harder constraint than the hints, and the workarea clamp wins when both
/// cannot be satisfied at once.
pub(crate) fn snap_float_to_hints(g: Rect, h: SizeHints) -> Rect {
    if !h.valid {
        return g;
    }
    let mut w = g.w as i32;
    let mut hh = g.h as i32;
    if h.min_w > 0 {
        w = w.max(h.min_w);
    }
    if h.min_h > 0 {
        hh = hh.max(h.min_h);
    }
    if h.max_w > 0 {
        w = w.min(h.max_w);
    }
    if h.max_h > 0 {
        hh = hh.min(h.max_h);
    }
    if h.inc_w > 0 {
        let base = h.base_w.max(0);
        // i64 math: hostile hints (`base = i32::MIN`) overflow `w - base`
        // in i32 (panic debug / wrap release) before the `.max(0)`.
        let n = (((w as i64 - base as i64).max(0) + h.inc_w as i64 / 2) / h.inc_w as i64)
            .min(i32::MAX as i64) as i32;
        w = base.saturating_add(n.saturating_mul(h.inc_w));
    }
    if h.inc_h > 0 {
        let base = h.base_h.max(0);
        let n = (((hh as i64 - base as i64).max(0) + h.inc_h as i64 / 2) / h.inc_h as i64)
            .min(i32::MAX as i64) as i32;
        hh = base.saturating_add(n.saturating_mul(h.inc_h));
    }
    // Final word: hard bounds (a client always accepts its own min/max).
    if h.min_w > 0 {
        w = w.max(h.min_w);
    }
    if h.min_h > 0 {
        hh = hh.max(h.min_h);
    }
    if h.max_w > 0 {
        w = w.min(h.max_w);
    }
    if h.max_h > 0 {
        hh = hh.min(h.max_h);
    }
    Rect::new(g.x, g.y, w.max(1) as u32, hh.max(1) as u32)
}

/// Floor a (workarea-clamped) size onto the increment grid without ever
/// growing it: the workarea clamp wins over the grid. A grid point is adopted
/// only when it still satisfies `min`; otherwise the clamped size is kept
/// (no grid point fits `[min, clamped]` — unsatisfiable constraints the
/// client must yield on, documented in `snap_float_to_hints`).
pub(crate) fn settle_to_grid(mut g: Rect, h: SizeHints) -> Rect {
    if !h.valid {
        return g;
    }
    if h.inc_w > 0 {
        let base = h.base_w.max(0);
        let w = g.w as i32;
        if w >= base {
            let floored = base.saturating_add(
                ((w as i64 - base as i64) / h.inc_w as i64 * h.inc_w as i64).min(i32::MAX as i64)
                    as i32,
            );
            if h.min_w <= 0 || floored >= h.min_w {
                g.w = floored.max(1) as u32;
            }
        }
    }
    if h.inc_h > 0 {
        let base = h.base_h.max(0);
        let hgt = g.h as i32;
        if hgt >= base {
            let floored = base.saturating_add(
                ((hgt as i64 - base as i64) / h.inc_h as i64 * h.inc_h as i64).min(i32::MAX as i64)
                    as i32,
            );
            if h.min_h <= 0 || floored >= h.min_h {
                g.h = floored.max(1) as u32;
            }
        }
    }
    g
}

/// Parse the body of a `WM_NORMAL_HINTS` property (the 18 `long`s of
/// `XSizeHints`) into `SizeHints`. Pure over the wire words so map-time
/// parsing and `PropertyNotify` refresh share one interpretation.
///
/// # Wire layout (ICCCM 4.1.2.3)
///
/// The property is the C `XSizeHints` structure serialized as CARD32s, so the
/// indices below are a *contract*, not a choice:
///
/// ```text
/// 0 flags        1 x          2 y          3 width     4 height
/// 5 min_w        6 min_h      7 max_w      8 max_h
/// 9 inc_w       10 inc_h     11 min_asp_x 12 min_asp_y
/// 13 max_asp_x  14 max_asp_y 15 base_w    16 base_h     17 win_gravity
/// ```
///
/// Every field is read from the index that `Xlib` itself uses (verified against
/// `XGetWMNormalHints`): a single-word slip silently feeds the float policy
/// garbage — a resize increment read out of the aspect slot is 0 (no snapping
/// at all), and a maximum read out of `min_aspect` clamps a 1:1-aspect client
/// to a few pixels, which the client then fights with a `ConfigureRequest` on
/// every frame.
///
/// Returns `None` when the body is short (< 18 words) — the caller keeps the
/// previous hints instead of installing a half-parsed constraint set.
pub(crate) fn parse_wm_normal_hints(v: &[u32]) -> Option<SizeHints> {
    if v.len() < 18 {
        return None;
    }
    let mut h = SizeHints::default();
    let f = v[0];
    if f & SizeHints::P_MIN_SIZE != 0 {
        h.min_w = v[5] as i32;
        h.min_h = v[6] as i32;
    }
    if f & SizeHints::P_MAX_SIZE != 0 {
        h.max_w = v[7] as i32;
        h.max_h = v[8] as i32;
    }
    if f & SizeHints::P_RESIZE_INC != 0 {
        h.inc_w = v[9] as i32;
        h.inc_h = v[10] as i32;
    }
    if f & SizeHints::P_ASPECT != 0 {
        // Each aspect ratio is `x / y`; a zero denominator is meaningless, so
        // it is neutralised to 1.
        h.min_aspect = v[11] as f32 / (v[12].max(1)) as f32;
        h.max_aspect = v[13] as f32 / (v[14].max(1)) as f32;
    }
    if f & SizeHints::P_BASE_SIZE != 0 {
        h.base_w = v[15] as i32;
        h.base_h = v[16] as i32;
    }
    h.flags = f;
    h.valid = true;
    Some(h)
}

/// True when the hints pin the window to exactly one size (`min == max` on
/// both axes). Such a window is always floated at manage time.
pub(crate) fn fixed_size_hints(h: &SizeHints) -> bool {
    h.valid && h.max_w > 0 && h.max_h > 0 && h.max_w == h.min_w && h.max_h == h.min_h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Cfg;
    use crate::types::{Client, Column, Edge, Focus, Monitor, Rect, WinFlags};

    /// Build a one-monitor state whose workarea is optionally inset on the left
    /// by a dock strut, with a single fullscreen window in a sole column.
    fn one_fs_state(left_strut: u32) -> State {
        let screen = Rect::new(0, 0, 1920, 1080);
        let mut mon = Monitor::new(screen, 1);
        if left_strut > 0 {
            mon.set_reserved_region(0xDEAD, Edge::Left, left_strut);
        }
        let mut state = State::new();
        state.monitors.push(mon);
        let ws = &mut state.monitors[0].workspaces[0];
        ws.columns.push(Column {
            windows: vec![1],
            focused: 0,
            weight: 1.0,
            boost: 1.0,
        });
        ws.focus = Focus { column_idx: 0 };
        let mut c = Client::new(1, 0, 0);
        c.border_w = 0;
        c.flags.set(WinFlags::FULLSCREEN);
        state.add_client(c);
        state
    }

    /// Arrange the active workspace and return the placements.
    fn place(state: &mut State, cfg: &Cfg) -> Placements {
        let fs = fs_ctx(
            &state.clients,
            state.monitors[0].ws(),
            state.monitors[0].screen,
        );
        let scroll = ideal_scroll(state.monitors[0].ws(), cfg, state.monitors[0].workarea, fs);
        state.monitors[0].workspaces[0].camera.position = scroll;
        state.monitors[0].workspaces[0].camera.target = scroll;
        let mut out = Placements::new();
        let mut scratch = RibbonScratch::default();
        arrange_columns(
            state,
            &state.monitors[0],
            cfg,
            Phase::Live,
            &mut out,
            &mut scratch,
        );
        out
    }

    #[test]
    fn fullscreen_column_fills_screen_when_centered() {
        let cfg = Cfg::default();
        let mut state = one_fs_state(0);
        let p = place(&mut state, &cfg);
        assert_eq!(p.len(), 1, "exactly the fullscreen window is placed");
        let (win, rect, bw) = p[0];
        assert_eq!(win, 1);
        assert_eq!(bw, 0, "fullscreen uses border 0");
        assert_eq!(
            rect, state.monitors[0].screen,
            "centered fullscreen must exactly fill the screen"
        );
    }

    #[test]
    fn fullscreen_column_aligns_to_screen_edge_with_asymmetric_struts() {
        let cfg = Cfg::default();
        // A left dock pushes the workarea right, but the fullscreen column must
        // still align to `screen.x` (0), not to `workarea.x`.
        let mut state = one_fs_state(120);
        let p = place(&mut state, &cfg);
        let (_, rect, _) = p[0];
        assert_eq!(
            rect.x, state.monitors[0].screen.x,
            "fullscreen left edge must equal screen.x even with a left strut"
        );
        assert_eq!(rect, state.monitors[0].screen);
    }

    #[test]
    fn fullscreen_column_scrolls_away() {
        let cfg = Cfg::default();
        let mut state = State::new();
        let screen = Rect::new(0, 0, 1920, 1080);
        state.monitors.push(Monitor::new(screen, 1));
        // Two columns: [fs col 0] [normal col 1].
        {
            let ws = &mut state.monitors[0].workspaces[0];
            ws.columns.push(Column {
                windows: vec![1],
                focused: 0,
                weight: 1.0,
                boost: 1.0,
            });
            ws.columns.push(Column {
                windows: vec![2],
                focused: 0,
                weight: 0.5,
                boost: 0.0,
            });
            ws.focus = Focus { column_idx: 0 };
        }
        let mut cf = Client::new(1, 0, 0);
        cf.flags.set(WinFlags::FULLSCREEN);
        cf.border_w = 0;
        state.add_client(cf);
        let cn = Client::new(2, 0, 0);
        state.add_client(cn);

        // Focused on the fullscreen column: it fills the screen.
        let fs0 = fs_ctx(&state.clients, state.monitors[0].ws(), screen);
        let scroll0 = ideal_scroll(
            state.monitors[0].ws(),
            &cfg,
            state.monitors[0].workarea,
            fs0,
        );
        state.monitors[0].workspaces[0].camera.position = scroll0;
        let mut out = Placements::new();
        let mut scratch = RibbonScratch::default();
        arrange_columns(
            &state,
            &state.monitors[0],
            &cfg,
            crate::core::layout::Phase::Live,
            &mut out,
            &mut scratch,
        );
        let (_, fs_rect_focused, _) = out.iter().find(|e| e.0 == 1).copied().unwrap();
        assert_eq!(
            fs_rect_focused.x, screen.x,
            "fullscreen on its own column aligns to screen.x"
        );

        // Focus moves to the neighbour column → the fullscreen scrolls away (its
        // left edge slides left of `screen.x`; it is no longer pinned on the
        // screen, which is exactly the niri behaviour).
        state.monitors[0].workspaces[0].focus.column_idx = 1;
        state.monitors[0].focused = Some(2);
        let fs1 = fs_ctx(&state.clients, state.monitors[0].ws(), screen);
        let scroll1 = ideal_scroll(
            state.monitors[0].ws(),
            &cfg,
            state.monitors[0].workarea,
            fs1,
        );
        state.monitors[0].workspaces[0].camera.position = scroll1;
        let mut out2 = Placements::new();
        let mut scratch = RibbonScratch::default();
        arrange_columns(
            &state,
            &state.monitors[0],
            &cfg,
            crate::core::layout::Phase::Live,
            &mut out2,
            &mut scratch,
        );
        let (_, fs_rect_away, _) = out2.iter().find(|e| e.0 == 1).copied().unwrap();
        assert!(
            fs_rect_away.x < screen.x,
            "fullscreen must scroll left (away) when a neighbour column is focused: {fs_rect_away:?}"
        );
    }

    #[test]
    fn fullscreen_hides_column_siblings() {
        let cfg = Cfg::default();
        let mut state = State::new();
        let screen = Rect::new(0, 0, 1920, 1080);
        state.monitors.push(Monitor::new(screen, 1));
        {
            let ws = &mut state.monitors[0].workspaces[0];
            // One column with two stacked windows, the first fullscreen.
            ws.columns.push(Column {
                windows: vec![1, 2],
                focused: 0,
                weight: 1.0,
                boost: 1.0,
            });
            ws.focus = Focus { column_idx: 0 };
        }
        let mut c1 = Client::new(1, 0, 0);
        c1.flags.set(WinFlags::FULLSCREEN);
        c1.border_w = 0;
        state.add_client(c1);
        state.add_client(Client::new(2, 0, 0));

        let fs = fs_ctx(&state.clients, state.monitors[0].ws(), screen);
        let scroll = ideal_scroll(state.monitors[0].ws(), &cfg, state.monitors[0].workarea, fs);
        state.monitors[0].workspaces[0].camera.position = scroll;
        let mut out = Placements::new();
        let mut scratch = RibbonScratch::default();
        arrange_columns(
            &state,
            &state.monitors[0],
            &cfg,
            crate::core::layout::Phase::Live,
            &mut out,
            &mut scratch,
        );

        assert_eq!(out.len(), 1, "only the fullscreen window is placed");
        assert_eq!(out[0].0, 1, "the sibling is hidden, not placed");
    }

    /// The three ribbon consumers must agree for a fullscreen column: two
    /// columns, col 0 fullscreen and focused. The placement's left edge,
    /// `column_screen_extents`' left edge and the centered/aligned camera all
    /// derive from `ribbon_geom`, the single geometry source of truth.
    #[test]
    fn ribbon_invariants_hold_with_fullscreen() {
        let cfg = Cfg::default();
        let mut state = State::new();
        let screen = Rect::new(50, 0, 1920, 1080); // asymmetric strut on the left
        let mut mon = Monitor::new(screen, 1);
        mon.set_reserved_region(0xDEAD, Edge::Left, 50);
        state.monitors.push(mon);
        {
            let ws = &mut state.monitors[0].workspaces[0];
            ws.columns.push(Column {
                windows: vec![1],
                focused: 0,
                weight: 1.0,
                boost: 1.0,
            });
            ws.columns.push(Column {
                windows: vec![2],
                focused: 0,
                weight: 0.5,
                boost: 0.0,
            });
            ws.focus = Focus { column_idx: 0 };
        }
        let mut c1 = Client::new(1, 0, 0);
        c1.flags.set(WinFlags::FULLSCREEN);
        c1.border_w = 0;
        state.add_client(c1);
        state.add_client(Client::new(2, 0, 0));

        let fs = fs_ctx(&state.clients, state.monitors[0].ws(), screen);
        let scroll = ideal_scroll(
            state.monitors[0].ws(),
            &cfg,
            state.monitors[0].workarea,
            fs.clone(),
        );
        state.monitors[0].workspaces[0].camera.position = scroll;
        state.monitors[0].workspaces[0].camera.target = scroll;

        let mut out = Placements::new();
        let mut scratch = RibbonScratch::default();
        arrange_columns(
            &state,
            &state.monitors[0],
            &cfg,
            crate::core::layout::Phase::Live,
            &mut out,
            &mut scratch,
        );
        let (_, rect, _) = out.iter().find(|e| e.0 == 1).copied().unwrap();

        let extents = column_screen_extents(
            state.monitors[0].ws(),
            &cfg,
            state.monitors[0].workarea,
            &fs,
        );

        // `column_screen_extents` agrees with the arrange placement.
        let (el, er) = extents[0];
        assert!(
            (el - rect.x as f32).abs() <= 2.0,
            "extents left {el} != arrange left {}",
            rect.x
        );
        assert!(
            (er - (rect.x + rect.w as i32) as f32).abs() <= 2.0,
            "extents right {er} != arrange right {}",
            rect.x + rect.w as i32
        );

        // The aligned camera target yields a fullscreen left edge equal to
        // `screen.x` (the whole point of the asymmetric-strut fix).
        assert_eq!(
            rect.x, screen.x,
            "fullscreen left must equal screen.x under the aligned camera; got {}",
            rect.x
        );
    }
    // Extreme layout inputs (giant gaps, tiny workarea, many columns) must
    // still produce valid geometry: no overflow, no negative projection
    // coordinates escaping the saturating math, no NaN/inf. The layout has to
    // survive degenerate inputs without panicking and without absurd rects.

    /// Build a state with `n` single-window columns on a `screen` monitor.
    fn many_columns_state(n: usize) -> State {
        let mut state = State::new();
        state
            .monitors
            .push(Monitor::new(Rect::new(0, 0, 1920, 1080), 9));
        let ws = &mut state.monitors[0].workspaces[0];
        for i in 0..n {
            let win = (i + 1) as u32;
            ws.columns.push(Column {
                windows: vec![win],
                focused: 0,
                weight: 1.0 / n as f32,
                boost: 0.0,
            });
        }
        for i in 0..n {
            state.add_client(Client::new((i + 1) as u32, 0, 0));
        }
        state.monitors[0].workspaces[0].focus = Focus { column_idx: 0 };
        state
    }

    fn assert_geometry_sane(state: &mut State, cfg: &Cfg) {
        let p = place(state, cfg);
        for (_, rect, _) in &p {
            assert!(rect.w < 1 << 30, "width overflowed: {rect:?}");
            assert!(rect.h < 1 << 30, "height overflowed: {rect:?}");
            assert!(rect.x.abs() < 1 << 24, "x exploded: {rect:?}");
            assert!(rect.y.abs() < 1 << 24, "y exploded: {rect:?}");
        }
    }

    #[test]
    fn giant_gaps_tiny_workarea_stay_valid() {
        let cfg = Cfg {
            // Gaps larger than the workarea itself.
            gaps_inner: 5_000,
            gaps_outer: 5_000,
            ..Cfg::default()
        };
        let mut state = many_columns_state(4);
        // Shrink the workarea below the gaps via a dock reservation.
        state.monitors[0].set_reserved_region(0xBEEF, crate::types::Edge::Left, 1900);
        assert_geometry_sane(&mut state, &cfg);
    }

    #[test]
    fn many_rows_and_columns_with_extreme_gaps_never_produce_invalid_geometry() {
        let cfg = Cfg {
            gaps_inner: 2_000,
            gaps_outer: 2_000,
            ..Cfg::default()
        };
        let mut state = many_columns_state(40);
        assert_geometry_sane(&mut state, &cfg);

        // A 1x1 workarea: every resource consumed by gaps/borders, geometry
        // must clamp instead of wrapping or going negative.
        let mut tiny = many_columns_state(3);
        tiny.monitors[0].screen = Rect::new(0, 0, 1, 1);
        tiny.monitors[0].workarea = Rect::new(0, 0, 1, 1);
        assert_geometry_sane(&mut tiny, &cfg);
    }

    /// Build a state with `cols` columns × `rows` windows per column on a
    /// `w`×`h` monitor.
    fn grid_state(w: u32, h: u32, cols: usize, rows: usize) -> State {
        let mut state = State::new();
        state.monitors.push(Monitor::new(Rect::new(0, 0, w, h), 9));
        let ws = &mut state.monitors[0].workspaces[0];
        let mut next_win: u32 = 1;
        for _ in 0..cols {
            ws.columns.push(Column {
                windows: (0..rows)
                    .map(|_| {
                        let id = next_win;
                        next_win += 1;
                        id
                    })
                    .collect(),
                focused: 0,
                weight: 1.0 / cols as f32,
                boost: 0.0,
            });
        }
        ws.focus = Focus { column_idx: 0 };
        for i in 0..(cols * rows) as u32 {
            state.add_client(Client::new(i + 1, 0, 0));
        }
        state
    }

    /// A geometry-validity predicate stronger than `assert_geometry_sane`:
    /// finite/reasonable coordinates, non-degenerate sizes, per-window fit
    /// inside the workarea, and vertical stacking that stays inside the
    /// workarea (or within the unavoidable 1px-per-window floor when the
    /// workarea is smaller than the row count — n windows of ≥ 1 px each
    /// simply cannot fit into fewer than n pixels; the layout must then stay
    /// valid and *bounded*, not escape to absurdity).
    ///
    /// Horizontal containment is intentionally NOT asserted: the column
    /// ribbon is scrollable by design, so a column lying outside the workarea
    /// on the x axis is legitimate (the camera scrolls to it), unlike a
    /// vertical escape which nothing can recover.
    fn assert_gaps_geometry_within_workarea(state: &mut State, cfg: &Cfg, rows: usize) {
        let wa = state.monitors[0].workarea;
        let p = place(state, cfg);
        assert!(!p.is_empty(), "every client must receive a placement");
        let one_px = |v: u32| (v as i32).max(1) as u32;
        for (win, rect, _) in &p {
            assert!(
                state.clients.contains_key(win),
                "placement emitted for an unknown client"
            );
            assert!(rect.x.abs() < 1 << 24, "x not reasonable: {rect:?}");
            assert!(rect.y.abs() < 1 << 24, "y not reasonable: {rect:?}");
            assert!(rect.w >= 1 && rect.h >= 1, "degenerate rectangle: {rect:?}");
            assert!(
                rect.w <= one_px(wa.w) && rect.h <= one_px(wa.h),
                "window larger than the workarea: {rect:?} wa={wa:?}"
            );
            let v_room = (wa.h as i32).max(rows as i32);
            assert!(rect.y >= wa.y, "row above the workarea: {rect:?} wa={wa:?}");
            assert!(
                rect.y + rect.h as i32 <= wa.y + v_room,
                "rows escaped the workarea: {rect:?} wa={wa:?}"
            );
        }
    }

    /// 1x1 workarea + 2 clients + huge gap: the most degenerate shape.
    /// Validity and a bounded, documented 1px-per-row floor are the only
    /// possible guarantees here.
    #[test]
    fn one_by_one_workarea_two_clients_huge_gap_stay_valid() {
        let cfg = Cfg {
            gaps_inner: 5_000,
            gaps_outer: 5_000,
            ..Cfg::default()
        };
        let mut state = grid_state(1, 1, 1, 2);
        assert_gaps_geometry_within_workarea(&mut state, &cfg, 2);
    }

    /// 100x100 workarea + 100 clients + huge gap: exactly one pixel per row —
    /// the vertical gap clamp must collapse to 0 and every row must land
    /// precisely inside the workarea.
    #[test]
    fn hundred_square_workarea_hundred_clients_huge_gap_stay_valid() {
        let cfg = Cfg {
            gaps_inner: 5_000,
            gaps_outer: 5_000,
            ..Cfg::default()
        };
        let mut state = grid_state(100, 100, 1, 100);
        assert_gaps_geometry_within_workarea(&mut state, &cfg, 100);
    }

    /// 1920x1080 + 3 clients + huge gap: a real monitor consumed entirely by
    /// gaps must clamp, not produce absurd rectangles.
    #[test]
    fn full_hd_three_clients_huge_gap_stay_valid() {
        let cfg = Cfg {
            gaps_inner: 5_000,
            gaps_outer: 5_000,
            ..Cfg::default()
        };
        let mut state = grid_state(1920, 1080, 1, 3);
        assert_gaps_geometry_within_workarea(&mut state, &cfg, 3);
    }

    /// Gap sweep: 0, the normal default, the clamp ceiling, and values above
    /// `i32::MAX` that would wrap NEGATIVE in the u32→i32 conversion and push
    /// the workarea in the wrong direction.
    #[test]
    fn gap_sweep_zero_normal_ceiling_and_beyond_i32_stay_valid() {
        for (inner, outer) in [
            (0u32, 0u32),
            (6, 6),
            (1_000_000, 1_000_000),
            (u32::MAX, u32::MAX),
        ] {
            let cfg = Cfg {
                gaps_inner: inner,
                gaps_outer: outer,
                ..Cfg::default()
            };
            let mut state = grid_state(1920, 1080, 2, 3);
            assert_gaps_geometry_within_workarea(&mut state, &cfg, 3);
        }
    }

    /// Border sweep over every `u32` a user can type in `border_width`:
    /// realistic widths, the clamp ceiling, the value that makes `2 * bw`
    /// overflow `i32`, and the two that wrap NEGATIVE through `u32 as i32`.
    ///
    /// The contract: the projection never panics and never emits a rectangle
    /// larger than the workarea — a border that wrapped negative would *add* the
    /// frame to a row instead of reserving it, which on a 1x1 workarea is a
    /// three-pixel window — and every placement reports the border actually
    /// reserved, never the raw config value.
    #[test]
    fn border_w_sweep_extreme_config_stays_valid() {
        let assert_valid = |state: &mut State, cfg: &Cfg, border_w: u32| {
            let wa = state.monitors[0].workarea;
            let p = place(state, cfg);
            assert!(!p.is_empty(), "every client must receive a placement");
            for (_, rect, bw) in &p {
                assert!(
                    rect.w >= 1 && rect.h >= 1,
                    "degenerate rectangle for border_w={border_w}: {rect:?}"
                );
                assert!(
                    rect.w <= wa.w && rect.h <= wa.h,
                    "window larger than the workarea for border_w={border_w}: \
                     {rect:?} wa={wa:?}"
                );
                assert!(
                    rect.x.abs() < 1 << 24 && rect.y.abs() < 1 << 24,
                    "coordinates escaped for border_w={border_w}: {rect:?}"
                );
                assert!(
                    *bw <= MAX_CFG_BORDER as u32,
                    "placement must report the reserved border, not the raw config \
                     value (border_w={border_w}, reported={bw})"
                );
            }
        };

        for border_w in [
            0u32,
            1,
            2,
            4,
            1_000_000,               // the ceiling
            1_000_001,               // first value the ceiling actually clamps
            i32::MAX as u32 / 2 + 1, // `2 * bw` no longer fits in `i32`
            i32::MAX as u32,
            u32::MAX, // `as i32` wraps to -1: the frame would be *added*
        ] {
            let cfg = Cfg {
                border_w,
                ..Cfg::default()
            };
            assert_valid(&mut grid_state(1920, 1080, 2, 3), &cfg, border_w);
            // A 1x1 workarea: one pixel per row, so any frame arithmetic that is
            // not exactly right shows up as a window that no longer fits.
            assert_valid(&mut grid_state(1, 1, 2, 2), &cfg, border_w);
        }
    }

    /// A `border_w` the user can actually type must reach the layout untouched:
    /// the placement reports exactly that border and keeps the geometry the
    /// unclamped projection produced, so nothing between the config and the
    /// rectangle rewrites an observable value.
    ///
    /// The expected rectangles are the projection's own output for a 1920x1080
    /// monitor with one column of two rows, pinned so a later bound cannot move
    /// a realistic border by a single pixel.
    #[test]
    fn realistic_border_w_is_passed_through_unchanged() {
        for (border_w, expected) in [
            (0u32, [(1, 8, 8, 1904, 530), (2, 8, 542, 1904, 530)]),
            (1, [(1, 8, 8, 1902, 528), (2, 8, 542, 1902, 528)]),
            (2, [(1, 8, 8, 1900, 526), (2, 8, 542, 1900, 526)]),
            (4, [(1, 8, 8, 1896, 522), (2, 8, 542, 1896, 522)]),
        ] {
            let cfg = Cfg {
                border_w,
                ..Cfg::default()
            };
            let mut state = grid_state(1920, 1080, 1, 2);
            let p = place(&mut state, &cfg);
            let got: Vec<_> = p
                .iter()
                .map(|&(win, rect, bw)| {
                    assert_eq!(
                        bw, border_w,
                        "the reported border must be the configured one (border_w={border_w})"
                    );
                    (win, rect.x, rect.y, rect.w, rect.h)
                })
                .collect();
            assert_eq!(got, expected, "border_w={border_w} changed the projection");
        }
    }
}
