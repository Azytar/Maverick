//! Columnar layout engine (niri-style) — pure coordinate computation.
//!
//! Owns `RibbonScratch`/`RibbonGeom`, `FsCtx`, and the
//! `arrange`/`arrange_columns` projection. Geometry is
//! *computed*, never stored: `arrange` is a pure function of `State` + `Cfg`
//! with no I/O, no X11 and no wall-clock, which is what makes the layout
//! testable without a display.
//!
//! `ribbon_geom` is the single geometry source: the arrange loop, the camera
//! target (`ideal_scroll`) and the hit-test extents (`column_screen_extents`)
//! all read the same table, so camera and hit-test cannot drift.
//!
//! There is one projection, not two. Geometry is never interpolated: a scroll
//! or a zoom rewrites the camera or the zoom factor and the next `arrange` is
//! the final geometry.
//!
//! Not owned here: the presentation overlay (`present::present_into` rewrites
//! placements after layout), the reconciler and `AppliedState`, and the X11
//! backend.

use std::collections::HashMap;

use crate::config::Cfg;
use crate::types::{
    Client, LayoutKind, Monitor, Rect, SizeHints, State, ViewportMode, WindowId, Workspace,
};

/// Scratch tuple-vec `(WindowId, Rect, border_w)` that `arrange` fills before
/// `DesiredState::from_placements` makes it explicit. Cleared on every call.
pub type Placements = Vec<(WindowId, Rect, u32)>; // (win, geom, border_w)

/// Reusable scratch for the per-monitor column projection.
///
/// `ribbon_geom` builds a per-column `(x, width)` table that the arrange loop
/// then reads back by column index. Building that table allocates a `Vec` every
/// call, and `arrange` runs once per monitor per reconcile — so the table is
/// owned here and reused across calls.
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
        fs: &FsCtx,
    ) -> RibbonGeom<'_> {
        ribbon_geom_into(ws, cfg, workarea, fs, &mut self.cols)
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
/// One projection per monitor per turn: the geometry every managed window
/// should have right now. There is no interpolated or "live" variant — the
/// ribbon scrolls by rewriting the camera and re-projecting, so this is always
/// the final geometry the WM writes to X.
pub fn arrange(
    state: &State,
    mon_idx: usize,
    cfg: &Cfg,
    out: &mut Placements,
    scratch: &mut RibbonScratch,
) {
    let Some(mon) = state.monitors.get(mon_idx) else {
        // Stale monitor index after hotplug: produce no placements instead
        // of panicking; the reconciler keeps the last applied frame.
        out.clear();
        return;
    };
    if mon.workspaces.get(mon.active_ws).is_none() {
        out.clear();
        return;
    }
    // `out` is the WM's *shared* `desired` buffer. It must be cleared, not
    // appended to: a stale placement gets re-applied by `apply_geom` and
    // physically re-shows a window that `hide_offscreen` just moved off-screen
    // — a fullscreen window on the previously active workspace would reappear
    // covering the current one.
    out.clear();
    arrange_columns(state, mon, cfg, out, scratch);
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

/// Convenience wrapper: builds its own scratch. Callers on the per-frame path
/// should use [`RibbonScratch::ribbon_geom`] with a reused buffer instead.
pub(crate) fn ribbon_geom(
    ws: &Workspace,
    cfg: &Cfg,
    workarea: Rect,
    fs: &FsCtx,
) -> RibbonGeomOwned {
    let mut scratch = RibbonScratch::default();
    let g = ribbon_geom_into(ws, cfg, workarea, fs, &mut scratch.cols);
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

    let alpha = ws.zoom.max(0.05);
    // Viewport zoom: when the workspace is in `Zoomed` mode the zoom factor is
    // `page_zoom` (which may be > 1 to *enlarge* the ribbon), not the Overview
    // `zoom`. They are kept separate on purpose — Overview zooms out
    // (`alpha < 1`), Viewport zooms in (`alpha > 1`). `ribbon_geom` has no
    // upper clamp on `alpha`, so the enlargement falls out for free.
    let alpha = if ws.viewport_mode == ViewportMode::Zoomed {
        ws.page_zoom.max(0.05)
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

    // Per-column accordion boost: the focused column is worth 1.0 and every
    // other column 0.0, so changing focus widens the focused column on the next
    // projection. In Overview the boost is forced to 0 so every column sits at
    // its base width and the strip fits all of them.
    let total_boost = cfg.accordion_boost.clamp(0.0, 0.9);
    let focus_i = ws.focus.column_idx;

    cols.clear();
    let mut x: f32 = 0.0;
    for (i, c) in ws.columns.iter().enumerate() {
        let boost = if ws.overview || i != focus_i {
            0.0
        } else {
            1.0
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
/// config file does).
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

    // Single source of truth: the ribbon geometry this monitor's windows are
    // placed from.
    let g = scratch.ribbon_geom(ws, cfg, full_wa, &fs);
    let wa = g.wa;

    // The camera offset is the geometry: whatever it holds is what X is told.
    let cam = ws.camera.position;

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
/// projection as `arrange_columns`, so a hit-test agrees with what is drawn
/// rather than with a stale world-space estimate. Test-only for now: the
/// Mod4+wheel step no longer hit-tests the column under the pointer.
#[cfg(test)]
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

/// Allocation-free variant. Test-only since the wheel stopped hit-testing the
/// column under the pointer; it pins the extents to `arrange`'s placement.
#[cfg(test)]
pub(crate) fn column_screen_extents_into(
    ws: &Workspace,
    cfg: &Cfg,
    workarea: Rect,
    fs: &FsCtx,
    out: &mut Vec<(f32, f32)>,
    scratch: &mut RibbonScratch,
) {
    let g = ribbon_geom_into(ws, cfg, workarea, fs, &mut scratch.cols);
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
/// be a different, active one).
pub fn ideal_scroll(ws: &Workspace, cfg: &Cfg, workarea: Rect, fs: FsCtx) -> f32 {
    let g = ribbon_geom(ws, cfg, workarea, &fs);
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
/// rect outside the workarea, and the final settle never grows the clamped
/// size, so the answer lands on the grid the client itself declares — or, where
/// no grid point fits under the clamp, on the size the client's own correction
/// asks for. Either way the answer is the fixed point of the *whole*
/// composition, not only of its last step: there is nothing left for the next
/// normalize to correct (a float that does not move generates no spurious
/// `ConfigureWindow`).
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
/// Order matters: the 1 px protocol floor, min/max clamp, increment round-snap
/// (nearest multiple of `(size - base)`, the rounding the drag path uses), then
/// min/max clamp again as the final word. Hard `[min, max]` bounds win over
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
    // The 1 px protocol floor (`ConfigureWindow` of 0 is `BadValue`, and the
    // server drops the request) is applied on *both* sides of the increment
    // round, and that is not redundant: flooring only afterwards lets the round
    // choose a grid line for a size of 0, and the 1 px the floor then leaves is
    // *off* the client's grid, so the next normalize rounds it again and the
    // float grows a whole increment one arrange late. Flooring first makes the
    // round answer for a size X can actually represent.
    let mut w = (g.w as i32).max(1);
    let mut hh = (g.h as i32).max(1);
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
/// only when it still satisfies `min`; otherwise no grid point fits
/// `[min, clamped]` (unsatisfiable constraints the client must yield on,
/// documented in `snap_float_to_hints`) and the size kept is the one the
/// client's own correction of it asks for — the only size the grid, the hard
/// bounds and the clamp hold at the same time.
///
/// # Why the fallback is not the clamped size
///
/// A workarea clamp can leave a size between two grid lines, and a client may
/// put its `min` above the lower grid line (`base = 12`, `inc = 11`,
/// `min = 113`, a workarea that hosts 116 px). Keeping such a size verbatim
/// looks like the safe choice — it is inside the workarea — but it is not a
/// fixed point of the projection: the *next* call's `snap_float_to_hints` rounds
/// it down onto the grid and the minimum pulls it back up, so the float moved a
/// second time (`116 -> 113 -> 113`) and one arrange per float emitted a
/// `ConfigureWindow` for a window that had not changed. The snap is idempotent,
/// so *its* answer is a size the next snap keeps; and since the settle only
/// shrinks, adopting it can never break the workarea containment the clamp
/// established.
///
/// `g` must be workarea-clamped ([`clamp_float_geom`]) for that containment to
/// carry over to the result.
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
    // The last word on agreement, shared with the snap the next call runs: a
    // hint-respecting toolkit accepts exactly this size without re-asserting
    // it, so adopting it (never growing past it, never past the workarea clamp
    // either) is what leaves the projection with nothing to correct. On the
    // healthy path it is a no-op — a size already on the grid the client
    // declared is its own correction.
    let corrected = snap_float_to_hints(g, h);
    g.w = g.w.min(corrected.w);
    g.h = g.h.min(corrected.h);
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
        let mut out = Placements::new();
        let mut scratch = RibbonScratch::default();
        arrange_columns(
            state,
            &state.monitors[0],
            cfg,
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
            });
            ws.columns.push(Column {
                windows: vec![2],
                focused: 0,
                weight: 0.5,
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
            });
            ws.columns.push(Column {
                windows: vec![2],
                focused: 0,
                weight: 0.5,
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

        let mut out = Placements::new();
        let mut scratch = RibbonScratch::default();
        arrange_columns(
            &state,
            &state.monitors[0],
            &cfg,
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

/// Property-based coverage of the arrangement contracts.
///
/// The generators build states through the *public* core API (`Monitor::new`,
/// `set_reserved_region`, `Client::new`, the `Workspace` view fields), so every
/// input is a state the core itself considers well formed; the properties then
/// assert what this module documents rather than what the code happens to do:
///
/// * the projection is total, pure and idempotent over a caller-owned buffer;
/// * the camera is an input, never a source of truth (core invariant C);
/// * every emitted rect is a rect X11 can carry (`w >= 1`, `h >= 1`);
/// * the gap/border ceilings hold for *every* `u32` a config file can carry,
///   and the outer-gap inset never leaves the workarea it insets;
/// * `ribbon_geom` stays the single geometry source, so the renderer, the
///   camera target and the hit-test extents cannot drift apart;
/// * the float policy separates the two authorities: an adopted client rect is
///   projected back verbatim, a WM-decided one is normalized idempotently.
#[cfg(test)]
mod proptests {
    use super::*;
    use crate::types::{Client, Column, Edge, Focus, Monitor, Rect, State, WinFlags};
    use proptest::prelude::*;

    /// A screen rect spanning the shapes a `RandR` report can produce: a
    /// typical 1080p panel, a 1x1 or 0x0 degenerate output, a very wide
    /// ultrawide, and a secondary monitor with a non-zero origin.
    fn screen_rect() -> impl Strategy<Value = Rect> {
        (
            -3840i32..=3840,
            -2160i32..=2160,
            prop_oneof![0u32..=1, 0u32..=64, 320u32..=3840, 8000u32..=16384],
            prop_oneof![0u32..=1, 0u32..=64, 240u32..=2160, 8000u32..=16384],
        )
            .prop_map(|(x, y, w, h)| Rect::new(x, y, w, h))
    }

    /// A user-configurable gap: the everyday values, the documented ceiling,
    /// the first value the ceiling clamps, and the `u32`s whose `u32 as i32`
    /// cast would wrap negative at the representation boundary.
    fn cfg_gap() -> impl Strategy<Value = u32> {
        prop_oneof![
            0u32..=16,
            Just(0),
            Just(1),
            100u32..=2_000,
            Just(MAX_CFG_GAP as u32),
            Just(MAX_CFG_GAP as u32 + 1),
            Just(i32::MAX as u32),
            Just(u32::MAX),
        ]
    }

    /// A user-configurable border width, swept over the same boundaries as
    /// [`cfg_gap`]: the ceiling, the first clamped value, and the `u32`s that
    /// make the `2 * bw` frame cost overflow `i32`.
    fn cfg_border() -> impl Strategy<Value = u32> {
        prop_oneof![
            0u32..=8,
            Just(MAX_CFG_BORDER as u32),
            Just(MAX_CFG_BORDER as u32 + 1),
            Just(i32::MAX as u32 / 2 + 1),
            Just(i32::MAX as u32),
            Just(u32::MAX),
        ]
    }

    /// A finite camera offset, including the fractional and negative values a
    /// mid-flight spring produces. `Camera`'s finiteness is a `State` invariant
    /// (core invariant C), so non-finite inputs are not generated here.
    fn camera_offset() -> impl Strategy<Value = f32> {
        prop_oneof![0.0f32..=1.0, -20000.0f32..=20000.0, 0.05f32..=4.0]
    }

    /// A dock reservation: a realistic thickness plus the `u32::MAX` a hostile
    /// `_NET_WM_STRUT_PARTIAL` can carry.
    fn reservation() -> impl Strategy<Value = (Edge, u32)> {
        (
            prop_oneof![
                Just(Edge::Top),
                Just(Edge::Bottom),
                Just(Edge::Left),
                Just(Edge::Right)
            ],
            prop_oneof![0u32..=120, 1000u32..=4000, Just(u32::MAX)],
        )
    }

    /// `WM_NORMAL_HINTS` as a client can declare them, including the hostile
    /// shapes the float policy has to survive: `min > max`, increments far
    /// larger than any workarea, and `base = i32::MIN`.
    fn size_hints() -> impl Strategy<Value = SizeHints> {
        (
            any::<bool>(),
            prop_oneof![0i32..=64, 1i32..=100_000, Just(i32::MIN), Just(i32::MAX)],
            prop_oneof![0i32..=64, 1i32..=100_000, Just(i32::MIN), Just(i32::MAX)],
            prop_oneof![0i32..=64, 1i32..=100_000, Just(i32::MIN), Just(i32::MAX)],
            prop_oneof![0i32..=64, 1i32..=100_000, Just(i32::MIN), Just(i32::MAX)],
            prop_oneof![0i32..=8, 1i32..=4_000, Just(i32::MIN), Just(i32::MAX)],
            prop_oneof![0i32..=8, 1i32..=4_000, Just(i32::MIN), Just(i32::MAX)],
            prop_oneof![0i32..=64, Just(i32::MIN), Just(i32::MAX)],
        )
            .prop_map(
                |(valid, min_w, min_h, max_w, max_h, inc_w, inc_h, base_w)| SizeHints {
                    valid,
                    min_w,
                    min_h,
                    max_w,
                    max_h,
                    inc_w,
                    inc_h,
                    base_w,
                    base_h: base_w.wrapping_neg(),
                    min_aspect: 0.0,
                    max_aspect: 0.0,
                    flags: 0,
                },
            )
    }

    /// One generated workspace: a ribbon of `(weight, rows, boost)` columns plus
    /// every piece of per-workspace view state the projection reads.
    #[derive(Debug, Clone)]
    struct Ribbon {
        screen: Rect,
        reserved: Vec<(Edge, u32)>,
        gaps_inner: u32,
        gaps_outer: u32,
        border_w: u32,
        smart_gaps: bool,
        accordion_boost: f32,
        columns: Vec<(f32, usize)>,
        focus_col: usize,
        cam: f32,
        /// Overview zoom. The documented range is `<= 1.0` — Overview zooms
        /// *out*, and 1.0 is "not zoomed".
        zoom: f32,
        overview: bool,
        /// The viewport zoom axis, orthogonal to `zoom`: `Zoomed` feeds
        /// `page_zoom` into `alpha` and a value above 1 deliberately enlarges
        /// the ribbon past the workarea.
        zoomed: bool,
        page_zoom: f32,
    }

    /// A column tree that is *always* a legal `Workspace`: weights inside the
    /// documented `[0.05, 1.0]` and a focus pointer that indexes the columns.
    /// Row counts reach 0 so the empty-column branch is exercised, and the
    /// column list itself may be empty, which is the zero-window workspace.
    fn ribbon() -> impl Strategy<Value = Ribbon> {
        let columns = prop::collection::vec((0.05f32..=1.0, 0usize..=6), 0..=8);
        (
            screen_rect(),
            prop::collection::vec(reservation(), 0..=2),
            cfg_gap(),
            cfg_gap(),
            cfg_border(),
            any::<bool>(),
            0.0f32..=1.0,
            columns,
            (camera_offset(), camera_offset()),
            (
                prop_oneof![0.05f32..=1.0, Just(1.0)],
                any::<bool>(),
                any::<bool>(),
                1.0f32..=4.0,
            ),
        )
            .prop_map(
                |(
                    screen,
                    reserved,
                    gaps_inner,
                    gaps_outer,
                    border_w,
                    smart_gaps,
                    accordion_boost,
                    columns,
                    (cam, focus_seed),
                    (zoom, overview, zoomed, page_zoom),
                )| {
                    // Derive the focus pointer from a generated value so it is
                    // not correlated with the column count the shrinker also
                    // touches, then clamp it into range.
                    let focus_col = (focus_seed.abs() as usize) % (columns.len() + 1);
                    Ribbon {
                        screen,
                        reserved,
                        gaps_inner,
                        gaps_outer,
                        border_w,
                        smart_gaps,
                        accordion_boost,
                        columns,
                        focus_col,
                        cam,
                        zoom,
                        overview,
                        zoomed,
                        page_zoom,
                    }
                },
            )
    }

    impl Ribbon {
        /// Materialize the workspace. Window ids are handed out in column order
        /// from 1 upwards and every one of them is registered in
        /// `state.clients`, so the projection has no stale references to
        /// filter — `arrange_never_places_an_unmanaged_window` covers that case
        /// on its own.
        fn state(&self) -> State {
            let mut state = State::new();
            state.monitors.push(Monitor::new(self.screen, 2));
            let mut next: WindowId = 1;
            {
                let ws = &mut state.monitors[0].workspaces[0];
                for &(weight, rows) in &self.columns {
                    let mut windows: Vec<WindowId> = Vec::with_capacity(rows);
                    for _ in 0..rows {
                        windows.push(next);
                        next += 1;
                    }
                    ws.columns.push(Column {
                        windows,
                        focused: 0,
                        weight,
                    });
                }
                ws.focus = Focus {
                    column_idx: self.effective_focus(),
                };
                ws.camera.position = self.cam;
                ws.zoom = self.zoom;
                ws.overview = self.overview;
                ws.viewport_mode = if self.zoomed {
                    ViewportMode::Zoomed
                } else {
                    ViewportMode::Normal
                };
                ws.page_zoom = self.page_zoom;
            }
            for &(edge, thickness) in &self.reserved {
                state.monitors[0].set_reserved_region(0xD0C, edge, thickness);
            }
            for win in 1..next {
                state.add_client(Client::new(win, 0, 0));
            }
            state
        }

        fn cfg(&self) -> Cfg {
            Cfg {
                gaps_inner: self.gaps_inner,
                gaps_outer: self.gaps_outer,
                border_w: self.border_w,
                smart_gaps: self.smart_gaps,
                accordion_boost: self.accordion_boost,
                ..Cfg::default()
            }
        }

        /// The number of windows the column tree asks for, whether or not they
        /// end up managed.
        fn tiled_windows(&self) -> usize {
            self.columns.iter().map(|c| c.1).sum()
        }

        /// The focus pointer after the projection's own clamping, i.e. the
        /// column the workspace actually rests on.
        fn effective_focus(&self) -> usize {
            self.focus_col.min(self.columns.len().saturating_sub(1))
        }

        /// The same ribbon with one extra single-window column appended, which
        /// is the "the user opened another window" transition. The focus
        /// pointer is pinned to the column it already named, so the new column
        /// is a pure addition rather than a focus change.
        fn with_extra_column(&self) -> Ribbon {
            let mut grown = self.clone();
            grown.focus_col = self.effective_focus();
            grown.columns.push((0.5, 1));
            grown
        }
    }

    /// Project monitor 0 of `state` through the public `arrange` entry point —
    /// the path the reconciler uses to write geometry to X.
    fn project(state: &State, cfg: &Cfg) -> Placements {
        let mut out = Placements::new();
        arrange(
            state,
            0,
            cfg,
            &mut out,
            &mut RibbonScratch::default(),
        );
        out
    }

    /// The gap-inset workarea the projection actually anchors geometry to. It
    /// is `ribbon_geom`'s `wa`, not `mon.workarea`: the outer gap is part of the
    /// layout, so containment is asserted against the area tiles are fitted
    /// into rather than against the area that area was derived from.
    fn inset_workarea(state: &State, cfg: &Cfg) -> Rect {
        let mon = &state.monitors[0];
        let fs = fs_ctx(&state.clients, mon.ws(), mon.screen);
        ribbon_geom(mon.ws(), cfg, mon.workarea, &fs).wa
    }

    /// `arrange` documents itself as idempotent over a caller-owned buffer: the
    /// the backend runs it once per monitor per turn into a buffer
    /// that already holds the previous frame. Every frame must therefore
    /// produce exactly the frame it would have produced into an empty buffer —
    /// a second run over the same buffer, or a run over a buffer still holding
    /// the previous frame's placements, must not append, reorder or drop
    /// anything.
    #[test]
    fn arrange_is_idempotent_over_a_reused_buffer() {
        proptest!(|(r in ribbon())| {
            let state = r.state();
            let cfg = r.cfg();
            let mut out: Placements = vec![(u32::MAX, Rect::new(-9, -9, 3, 3), 7)];
            arrange(
                &state,
                0,
                &cfg,
                &mut out,
                &mut RibbonScratch::default(),
            );
            let first = out.clone();
            prop_assert_eq!(
                &out,
                &project(&state, &cfg),
                "a dirty buffer must not leak the previous frame into the projection"
            );
            arrange(
                &state,
                0,
                &cfg,
                &mut out,
                &mut RibbonScratch::default(),
            );
            prop_assert_eq!(&out, &first, "arranging twice over one buffer must not accumulate");
        });
    }

    /// A hotplugged-away monitor index is a documented outcome, not a panic:
    /// `arrange` clears the buffer and emits nothing, so the reconciler keeps
    /// the last applied frame instead of configuring ghost geometry. The buffer
    /// has to be cleared on that path too — leaking the previous monitor's
    /// placements is exactly the "a fullscreen window reappears over the
    /// current workspace" failure the clear exists for.
    #[test]
    fn a_stale_monitor_index_clears_the_buffer_instead_of_panicking() {
        proptest!(|(r in ribbon())| {
            let state = r.state();
            let cfg = r.cfg();
            let stale = state.monitors.len();
            let mut out = project(&state, &cfg);
            arrange(
                &state,
                stale,
                &cfg,
                &mut out,
                &mut RibbonScratch::default(),
            );
            prop_assert!(
                out.is_empty(),
                "a stale monitor index must place nothing, left {:?}",
                out
            );
        });
    }



    /// `Rect`'s contract is `w >= 1 && h >= 1` for every arranged window: a
    /// `ConfigureWindow` with a zero extent is `BadValue`, the server drops the
    /// request, and `AppliedState` drifts ahead of reality with no event left to
    /// correct it. Every tiling shape, gap and border the config can carry must
    /// still floor at one pixel.
    #[test]
    fn no_arranged_rect_is_degenerate() {
        proptest!(|(r in ribbon())| {
            let state = r.state();
            let cfg = r.cfg();
            {
                for (win, rect, bw) in project(&state, &cfg) {
                    prop_assert!(
                        rect.w >= 1 && rect.h >= 1,
                        "a degenerate rect for window {}: {:?} (bw={})",
                        win,
                        rect,
                        bw
                    );
                }
            }
        });
    }

    /// The outer gap is the one layout input that anchors real geometry: it
    /// insets the workarea on all four edges, so a gap larger than half the
    /// workarea would push the inset area off the monitor entirely (a window at
    /// y=5000 on a 1080 px display). The projection clamps it to the largest gap
    /// that keeps the inset inside the workarea, so the inset area is contained
    /// in the workarea for *every* gap — including the `u32::MAX` that would
    /// otherwise wrap negative at the `u32 as i32` boundary.
    #[test]
    fn the_outer_gap_never_pushes_the_inset_off_the_workarea() {
        proptest!(|(r in ribbon())| {
            let state = r.state();
            let cfg = r.cfg();
            let wa = state.monitors[0].workarea;
            let inset = inset_workarea(&state, &cfg);
            prop_assert!(
                inset.x >= wa.x && inset.y >= wa.y,
                "the gap inset escaped the workarea origin: inset={:?} wa={:?}",
                inset,
                wa
            );
            prop_assert!(
                inset.right() <= wa.right() && inset.bottom() <= wa.bottom(),
                "the gap inset escaped the workarea: inset={:?} wa={:?}",
                inset,
                wa
            );
        });
    }

    /// A user-supplied `border_w` reaches the layout from the config file
    /// without validation, so it is only safe because of the documented ceiling: past `i32::MAX / 2` the `2 * bw`
    /// the frame costs overflows `i32`, and past `i32::MAX` the `u32 as i32`
    /// cast wraps negative and *adds* the frame to a row instead of reserving
    /// it. The placement must therefore report exactly the clamped value — a
    /// window configured with a border its geometry was not computed from gets
    /// a frame that does not match its rect — and the geometry must stay valid
    /// and bounded for every input on both sides of the ceiling.
    #[test]
    fn every_accepted_border_width_preserves_the_geometry_contract() {
        proptest!(|(r in ribbon())| {
            // The viewport zoom enlarges the ribbon past the workarea on
            // purpose; the border contract is about the tiling path.
            prop_assume!(!r.zoomed);
            let state = r.state();
            let cfg = r.cfg();
            let expected = cfg.border_w.min(MAX_CFG_BORDER as u32);
            let wa = inset_workarea(&state, &cfg);
            {
                for (win, rect, bw) in project(&state, &cfg) {
                    prop_assert_eq!(
                        bw,
                        expected,
                        "reported a border the geometry was not computed from \
                         (window {}, cfg.border_w={}, reported={})",
                        win,
                        cfg.border_w,
                        bw
                    );
                    prop_assert!(
                        rect.w >= 1 && rect.h >= 1,
                        "border_w={} degenerated the rect of window {}: {:?}",
                        cfg.border_w,
                        win,
                        rect
                    );
                    prop_assert!(
                        rect.w <= wa.w.max(1) && rect.h <= wa.h.max(1),
                        "border_w={} made window {} larger than the area it is fitted into: \
                         {:?} wa={:?}",
                        cfg.border_w,
                        win,
                        rect,
                        wa
                    );
                }
            }
        });
    }

    /// Tiles are stacked into the gap-inset workarea, and the vertical gap is
    /// clamped so a run of rows can never be pushed out of it. The documented
    /// exceptions are the viewport zoom, which deliberately enlarges the ribbon
    /// past the workarea, and the one-pixel-per-row floor: a workarea shorter
    /// than the row count cannot fit `n` windows of at least a pixel each, so
    /// the stack may exceed the area by up to one pixel per row — and by no
    /// more.
    ///
    /// Horizontal containment is deliberately *not* asserted: the ribbon
    /// scrolls, so a column sitting outside the workarea on x is the camera's
    /// job, not a layout error (see `Workspace::camera` and `ideal_scroll`).
    #[test]
    fn tiles_stay_within_the_gap_inset_workarea_on_the_vertical_axis() {
        proptest!(|(r in ribbon())| {
            // `ViewportMode::Zoomed` with `page_zoom > 1` exists to enlarge the
            // ribbon past the workarea, so containment is a property of the
            // tiling path this module owns.
            prop_assume!(!r.zoomed);
            let state = r.state();
            let cfg = r.cfg();
            let wa = inset_workarea(&state, &cfg);
            for (win, rect, _) in project(&state, &cfg) {
                // The floor is one pixel per row of the column this window is
                // in; the generous bound is `tiled_windows`, which every column
                // fits under.
                let floor = (r.tiled_windows() as i32).max(1);
                prop_assert!(
                    rect.y >= wa.y - 1,
                    "window {} starts above the area it is fitted into: {:?} wa={:?}",
                    win,
                    rect,
                    wa
                );
                prop_assert!(
                    rect.bottom() <= wa.bottom() + floor,
                    "window {} escaped the bottom of the area it is fitted into: {:?} wa={:?} \
                     (rows per column={:?})",
                    win,
                    rect,
                    wa,
                    r.columns.iter().map(|c| c.1).collect::<Vec<_>>()
                );
            }
        });
    }

    /// `smart_gaps` is a documented user preference with an exact contract:
    /// "collapse to 0 when exactly one tiled window" and no floats. With the
    /// collapse in effect the lone tile fills the whole workarea at rest; with
    /// the configured gaps it is inset by the outer gap instead. Both branches
    /// are asserted, so a preference that stops collapsing is caught as well as
    /// one that stops being honoured.
    #[test]
    fn smart_gaps_hand_a_lone_window_the_whole_gap_inset_workarea() {
        proptest!(|(screen in screen_rect(),
                     inner in cfg_gap(),
                     outer in cfg_gap(),
                     smart in any::<bool>())| {
            // No border: the assertion is about the gap, and a frame reserved
            // on both sides would legitimately shrink the tile.
            let cfg = Cfg {
                gaps_inner: inner,
                gaps_outer: outer,
                border_w: 0,
                smart_gaps: smart,
                ..Cfg::default()
            };
            let mut state = State::new();
            state.monitors.push(Monitor::new(screen, 2));
            state.monitors[0].workspaces[0].columns.push(Column {
                windows: vec![1],
                focused: 0,
                weight: 1.0,
            });
            state.add_client(Client::new(1, 0, 0));

            let p = project(&state, &cfg);
            prop_assert_eq!(p.len(), 1, "a lone tiled window must be placed");
            let (_, rect, _) = p[0];
            let workarea = state.monitors[0].workarea;
            let wa = inset_workarea(&state, &cfg);
            // The tile takes the whole inset area, up to the protocol floor of
            // one pixel that applies when the area itself is empty.
            prop_assert_eq!(
                (rect.y, rect.h),
                (wa.y, wa.h.max(1)),
                "a lone window must fill the gap-inset workarea: rect={:?} wa={:?}",
                rect,
                wa
            );
            if smart {
                prop_assert_eq!(
                    (wa.y, wa.h),
                    (workarea.y, workarea.h),
                    "smart_gaps must collapse the gap for a lone window: wa={:?} workarea={:?}",
                    wa,
                    workarea
                );
            } else {
                // The configured outer gap is still honoured whenever the
                // workarea is wide and tall enough for it to inset something.
                let twice = i64::from(outer) * 2 + 1;
                prop_assume!(
                    outer > 0
                        && i64::from(workarea.h) > twice
                        && i64::from(workarea.w) > twice
                );
                prop_assert!(
                    wa.h < workarea.h,
                    "gaps_outer={} was not applied to a lone window: wa={:?} workarea={:?}",
                    outer,
                    wa,
                    workarea
                );
            }
        });
    }

    /// Every column draws its width from a *fraction of the full workarea
    /// width*, independent of how many columns exist: opening another window
    /// grows the ribbon and the camera scrolls to it, instead of shrinking the
    /// windows already on screen. The widths and x positions of the pre-existing
    /// columns are therefore invariants, not artifacts of the column count.
    #[test]
    fn adding_a_column_leaves_the_existing_columns_untouched() {
        proptest!(|(r in ribbon())| {
            // `smart_gaps` is documented to collapse the gap at exactly one
            // tiled window, so appending a column legitimately changes the gap
            // itself; the column-count independence asserted here is about the
            // fixed-gap configuration.
            prop_assume!(!r.smart_gaps);
            let state = r.state();
            let cfg = r.cfg();
            let base = project(&state, &cfg);

            let grown = r.with_extra_column().state();
            let after = project(&grown, &cfg);
            prop_assert!(
                after.len() >= base.len(),
                "appending a column dropped placements: {} -> {}",
                base.len(),
                after.len()
            );
            prop_assert_eq!(
                &base[..],
                &after[..base.len()],
                "an extra column must not reflow the columns before it"
            );

            // The same independence in world space, where the camera has not
            // been applied yet.
            let mon = &state.monitors[0];
            let before = ribbon_geom(
                mon.ws(),
                &cfg,
                mon.workarea,
                &fs_ctx(&state.clients, mon.ws(), mon.screen),
            );
            let gmon = &grown.monitors[0];
            let gws = gmon.ws();
            let after = ribbon_geom(
                gws,
                &cfg,
                gmon.workarea,
                &fs_ctx(&grown.clients, gws, gmon.screen),
            );
            prop_assert_eq!(
                &before.cols[..],
                &after.cols[..before.cols.len()],
                "column widths are a fraction of the workarea, not of the column count"
            );
        });
    }

    /// A column with a legal weight (the documented `[0.05, 1.0]`) always gets
    /// a positive share of the workarea and never more than the whole of it:
    /// the accordion boost is a *bonus* on top of the base weight, so a boost
    /// of up to 0.9 must not push a column past the workarea it lives in.
    #[test]
    fn every_column_gets_a_bounded_positive_share_of_the_workarea() {
        proptest!(|(r in ribbon())| {
            let state = r.state();
            let cfg = r.cfg();
            let mon = &state.monitors[0];
            let g = ribbon_geom(
                mon.ws(),
                &cfg,
                mon.workarea,
                &fs_ctx(&state.clients, mon.ws(), mon.screen),
            );
            prop_assert_eq!(g.cols.len(), r.columns.len());
            for (i, &(_, w)) in g.cols.iter().enumerate() {
                let weight = r.columns[i].0;
                // A zero-extent workarea has no width to share; every column
                // must still get a positive share of whatever width there is.
                prop_assume!(g.wa.w > 0);
                prop_assert!(
                    w.is_finite() && w > 0.0,
                    "column {} (weight={}) got no share of the workarea",
                    i,
                    weight
                );
                prop_assert!(
                    w <= g.wa.w as f32 + f32::EPSILON,
                    "column {} (weight={}, rows={}) is wider than the workarea: {} > {}",
                    i,
                    weight,
                    r.columns[i].1,
                    w,
                    g.wa.w
                );
            }
        });
    }

    /// The column tree and `state.clients` are updated by independent event
    /// paths, so a window id can be referenced by a column that no longer owns
    /// a client. Such a window must receive no placement at all — projecting a
    /// rect for it would make the reconciler configure a window it does not
    /// manage — and no placement may ever name an id absent from `clients`.
    #[test]
    fn arrange_never_places_an_unmanaged_window() {
        proptest!(|(r in ribbon(), ghost in 0u32..=4)| {
            prop_assume!(r.tiled_windows() > 0);
            let mut state = r.state();
            let cfg = r.cfg();
            let ghost_id = u32::MAX - ghost;
            let ws = &mut state.monitors[0].workspaces[0];
            if let Some(col) = ws.columns.iter_mut().find(|c| !c.windows.is_empty()) {
                col.windows.push(ghost_id);
            }
            {
                for (win, _, _) in project(&state, &cfg) {
                    prop_assert!(
                        state.clients.contains_key(&win),
                        "placed window {}, which is not a managed client",
                        win
                    );
                    prop_assert_ne!(win, ghost_id, "a stale tree reference was projected");
                }
            }
        });
    }

    /// A workarea change must re-derive the scroll target.
    ///
    /// The camera is a pixel offset, not a fraction: it is only meaningful
    /// against the ribbon it was computed for, so any event that changes the
    /// workarea invalidates it. A value that is not re-derived afterwards points
    /// into a ribbon that no longer exists. That is not cosmetic. Park the camera
    /// on the last of twelve full-width columns, then shrink the screen the way a
    /// real `RandR` mode change does, and the target lands thousands of pixels past
    /// the end of the shorter ribbon: the focused column is drawn off the side of
    /// the monitor, and because `hide_offscreen` never parks the *active*
    /// workspace, every other window stays mapped too. The user is left with an
    /// empty desktop that nothing recovers, until an unrelated focus, grow,
    /// workspace or dock command happens to call `ideal_scroll` itself.
    ///
    /// The oracle is the observable — does the focused window still land inside
    /// the monitor — so it holds whatever mechanism re-derives the target. A dock
    /// strut, a resolution change and a monitor hotplug all have to satisfy it,
    /// and only the first two did.
    #[test]
    fn a_workarea_change_leaves_the_focused_column_on_the_screen() {
        let r = Ribbon {
            screen: Rect::new(0, 0, 1920, 1080),
            reserved: vec![],
            gaps_inner: 4,
            gaps_outer: 0,
            border_w: 2,
            smart_gaps: false,
            accordion_boost: 0.0,
            // Twelve full-width columns, so the ribbon is twelve screens long and
            // the camera is genuinely scrolled away from the origin.
            columns: vec![(1.0, 1); 12],
            focus_col: 11,
            cam: 0.0,
            zoom: 1.0,
            overview: false,
            zoomed: false,
            page_zoom: 1.0,
        };
        let mut state = r.state();
        let cfg = r.cfg();

        // Park the camera on the focus the way every command does, so the target
        // under test is a real one rather than a free draw.
        let old_target = {
            let State {
                clients, monitors, ..
            } = &mut state;
            let mon = &mut monitors[0];
            let fs = fs_ctx(clients, mon.ws(), mon.screen);
            let want = ideal_scroll(mon.ws(), &cfg, mon.workarea, fs);
            mon.ws_mut().camera.snap(want);
            want
        };
        let focused = state.monitors[0]
            .ws()
            .focused_win()
            .expect("a column is focused");
        assert!(
            old_target > 1_000.0,
            "fixture must leave the camera genuinely scrolled, got {old_target}"
        );

        // A RandR mode change: the screen moves and `recalc_geometry` is the only
        // writer — the two statements the RandR handler performs, and then it
        // re-derives the target, which is the whole of the fix.
        let old_screen = state.monitors[0].screen;
        let new_screen = Rect::new(0, 0, 800, 1080);
        {
            let State {
                clients, monitors, ..
            } = &mut state;
            monitors[0].screen = new_screen;
            monitors[0].recalc_geometry();
            let mon = &mut monitors[0];
            let fs = fs_ctx(clients, mon.ws(), mon.screen);
            let want = ideal_scroll(mon.ws(), &cfg, mon.workarea, fs);
            mon.ws_mut().camera.retarget(want);
        }

        assert!(
            state.monitors[0].ws().camera.position < old_target,
            "a shorter ribbon must target a smaller offset, not keep {old_target}"
        );

        let mut placements = Vec::new();
        let mut scratch = RibbonScratch::default();
        arrange(
            &state,
            0,
            &cfg,
            &mut placements,
            &mut scratch,
        );
        let rect = placements
            .iter()
            .find(|p| p.0 == focused)
            .map(|p| p.1)
            .expect("the focused window of the active workspace is placed");
        assert!(
            rect.right() > new_screen.x && rect.x < new_screen.right(),
            "the focused window is at {rect:?} after {}x{} -> {}x{}: entirely off \
             the monitor, so the desktop looks empty while every window is mapped",
            old_screen.w,
            old_screen.h,
            new_screen.w,
            new_screen.h
        );
    }

    /// `ideal_scroll` exists for one reason: the focused column must end up
    /// fully visible. The projection it feeds maps a camera offset `cam` to the
    /// world span `[cam - cx/alpha, cam - cx/alpha + wa.w/alpha]`, so a correct
    /// target always contains the focused column's world span once it has been
    /// clamped to `[cam_min, cam_max]`.
    #[test]
    fn the_camera_target_keeps_the_focused_column_visible() {
        proptest!(|(r in ribbon())| {
            let state = r.state();
            let cfg = r.cfg();
            let mon = &state.monitors[0];
            let fs = fs_ctx(&state.clients, mon.ws(), mon.screen);
            let g = ribbon_geom(mon.ws(), &cfg, mon.workarea, &fs);
            prop_assume!(!g.cols.is_empty());
            let cam = ideal_scroll(mon.ws(), &cfg, mon.workarea, fs.clone());
            prop_assert!(cam.is_finite(), "the camera target must be finite, got {}", cam);
            let visible_left = cam - g.cx / g.alpha;
            let visible_right = visible_left + g.wa.w as f32 / g.alpha;
            let i = mon.ws().focus.column_idx.min(g.cols.len() - 1);
            let (x, w) = g.cols[i];
            // A fullscreen column is `screen.w` wide and is targeted at
            // `screen.x` rather than centred in the workarea, so it is checked
            // for overlap only; the placement-level agreement is asserted by
            // the hit-test property instead.
            if !fs.cols.contains(&i) {
                // Whenever the column is narrow enough to fit in the visible
                // world span, the target must reveal all of it — that is the
                // whole point of `ideal_scroll`.
                let span = g.wa.w as f32 / g.alpha;
                if w <= span + 1.0 {
                    prop_assert!(
                        x >= visible_left - 1.0 && x + w <= visible_right + 1.0,
                        "the focused column fits but is not fully visible: col={} \
                         span=({}, {}) visible=({}, {}) cam={} alpha={} wa={:?}",
                        i,
                        x,
                        x + w,
                        visible_left,
                        visible_right,
                        cam,
                        g.alpha,
                        g.wa
                    );
                }
            }
            // A column wider than the view can never be fully visible, but the
            // target must still overlap it: clamped to the left bound the view
            // starts at world 0, clamped to the right bound it ends at
            // `total_w`, so the focused column is never scrolled off entirely.
            prop_assert!(
                x <= visible_right + 1.0 && x + w >= visible_left - 1.0,
                "the focused column was scrolled off screen: col={} span=({}, {}) \
                 visible=({}, {}) cam={} alpha={} wa={:?} total_w={}",
                i,
                x,
                x + w,
                visible_left,
                visible_right,
                cam,
                g.alpha,
                g.wa,
                g.total_w
            );
        });
    }

    /// `ribbon_geom` is the single geometry source: the renderer
    /// (`arrange_columns`), the camera target (`ideal_scroll`) and the hit-test
    /// extents (`column_screen_extents`) all read the same table, and the
    /// extents are documented to match what is actually drawn — the right edge
    /// is the *inner* width, the border already subtracted, because that is the
    /// rect X11 hit-tests against. Any drift leaves the Mod4+wheel stepping to a
    /// column the pointer is not over.
    #[test]
    fn the_hit_test_extents_agree_with_the_drawn_placement() {
        proptest!(|(r in ribbon())| {
            let state = r.state();
            let cfg = r.cfg();
            let placements = project(&state, &cfg);
            let mon = &state.monitors[0];
            let fs = fs_ctx(&state.clients, mon.ws(), mon.screen);
            let extents = column_screen_extents(mon.ws(), &cfg, mon.workarea, &fs);
            prop_assert_eq!(extents.len(), mon.ws().columns.len());

            // Placements arrive column-major: each column's managed windows in
            // order, then the floats. A fullscreen column contributes only its
            // own tile, so it consumes a single slot.
            let mut idx = 0usize;
            for (ci, col) in mon.ws().columns.iter().enumerate() {
                let slots = if fs.cols.contains(&ci) { 1 } else { col.windows.len() };
                let rows: Vec<(WindowId, Rect)> = placements[idx..]
                    .iter()
                    .take(slots)
                    .map(|&(win, rect, _)| (win, rect))
                    .collect();
                idx += rows.len();
                let (l, right) = extents[ci];
                for &(win, rect) in &rows {
                    if !col.windows.contains(&win) {
                        continue;
                    }
                    prop_assert!(
                        (l - rect.x as f32).abs() <= 2.0,
                        "column {} (window {}): hit-test left {} != drawn left {}",
                        ci,
                        win,
                        l,
                        rect.x
                    );
                    prop_assert!(
                        (right - (rect.x as f32 + rect.w as f32)).abs() <= 2.0,
                        "column {} (window {}): hit-test right {} != drawn right {} \
                         (cfg.border_w={})",
                        ci,
                        win,
                        right,
                        rect.x + rect.w as i32,
                        cfg.border_w
                    );
                }
            }
        });
    }

    /// `adopt_client_float_geometry` is the WM's promise to a client that asked
    /// for a geometry: it keeps whatever X11 can actually represent — `w`/`h`
    /// inside `1..=u16::MAX`, `x`/`y` inside the `i16` range `ConfigureNotify`
    /// transports — and changes nothing else. For a rect that is already
    /// representable the function must be the identity, because any rewrite
    /// restarts the configure ping-pong with the client.
    #[test]
    fn adopting_a_client_float_is_the_identity_for_representable_rects() {
        proptest!(|(x in i16::MIN as i32..=i16::MAX as i32,
                     y in i16::MIN as i32..=i16::MAX as i32,
                     w in 1u32..=u16::MAX as u32,
                     h in 1u32..=u16::MAX as u32)| {
            let g = Rect::new(x, y, w, h);
            prop_assert_eq!(adopt_client_float_geometry(g), g);
        });
    }

    /// The WM-side float policy is `snap_float_to_hints` → `clamp_float_geom` →
    /// `settle_to_grid`, and its output has to be a rect the workarea and the
    /// protocol both accept: never degenerate (`w`/`h >= 1`, a zero extent is
    /// `BadValue`) and never outside the workarea the clamp reserved the
    /// `2 * border_w` frame in. Both hold for every hint set a client can
    /// declare, including hostile ones (`base = i32::MIN`, `min > max`,
    /// increments larger than the workarea).
    #[test]
    fn the_wm_float_normalization_stays_inside_the_workarea() {
        proptest!(|(wa in screen_rect(), h in size_hints(), bw in 0u32..=64, g in screen_rect())| {
            let once = normalize_float_geom(g, h, wa, bw);
            prop_assert!(
                once.w >= 1 && once.h >= 1,
                "a degenerate float rect was projected: {:?} -> {:?} (hints={:?} bw={})",
                g,
                once,
                h,
                bw
            );
            prop_assert!(
                once.x >= wa.x && once.y >= wa.y,
                "a float escaped the workarea origin: {:?} -> {:?} wa={:?}",
                g,
                once,
                wa
            );
            prop_assert!(
                once.w == 1 || once.right() <= wa.right(),
                "a float escaped the workarea's right edge: {:?} -> {:?} wa={:?}",
                g,
                once,
                wa
            );
            prop_assert!(
                once.h == 1 || once.bottom() <= wa.bottom(),
                "a float escaped the workarea's bottom edge: {:?} -> {:?} wa={:?}",
                g,
                once,
                wa
            );
        });
    }

    /// `normalize_float_geom` is documented as idempotent — `f(f(x)) == f(x)` —
    /// and the float-stability argument rests on it: a float that has not moved
    /// must project to the identical rect on every arrange, or the reconciler
    /// emits a `ConfigureWindow` for a window that did not change, which is
    /// the "float that moves on its own" the policy exists to prevent.
    ///
    /// The domain includes a zero-extent rect, which is not hypothetical: a
    /// freshly managed client carries `Rect::default()` until the WM assigns a
    /// geometry, and `State::check_invariants` deliberately does not reject it
    /// ("valid intermediate states ... legitimately carry a default rect").
    ///
    /// The two ways this used to fail are pinned deterministically below
    /// (`the_clamp_and_the_min_hint_agree_on_one_size`,
    /// `the_protocol_floor_lands_on_the_client_grid`); a random hint set is a
    /// weak oracle for either, so the seeds this property shrunk to are kept in
    /// `proptest-regressions/core/layout.txt`.
    #[test]
    fn the_wm_float_normalization_is_a_fixed_point() {
        proptest!(|(wa in screen_rect(), h in size_hints(), bw in 0u32..=64, g in screen_rect())| {
            let once = normalize_float_geom(g, h, wa, bw);
            let twice = normalize_float_geom(once, h, wa, bw);
            prop_assert_eq!(
                once,
                twice,
                "the float projection is not a fixed point: {:?} -> {:?} -> {:?} (hints={:?} bw={})",
                g,
                once,
                twice,
                h,
                bw
            );
        });
    }

    /// The reported interaction, pinned: a client that publishes base / inc /
    /// min / max (xterm, foot, most GTK dialogs) inside a workarea too small for
    /// its `max` — a dock reservation, a small panel. The `2 * border_w` frame
    /// leaves 116 px of height, which is not on the client's 11 px grid, and the
    /// grid line below it (111) is under the client's own `min` of 113, so the
    /// grid and the minimum cannot both be honoured inside the workarea.
    ///
    /// The canonical answer is therefore 113: the one size under the clamp that
    /// the grid, the hard bounds and the workarea all hold at once. Keeping the
    /// clamped 116 (it is inside the workarea, which is what made it look safe)
    /// is what used to cost a second configure and a 3 px jump one arrange
    /// later, because the next call's snap rounded it onto the grid and the
    /// minimum pulled it back up: `116 -> 113 -> 113`.
    #[test]
    fn the_clamp_and_the_min_hint_agree_on_one_size() {
        let h = SizeHints {
            base_h: 12,
            inc_h: 11,
            max_h: 118,
            min_h: 113,
            valid: true,
            ..SizeHints::default()
        };
        let wa = Rect::new(0, 0, 0, 124);
        let bw = 4;
        let once = normalize_float_geom(Rect::new(0, 0, 0, 118), h, wa, bw);
        assert_eq!(once.h, 113, "the minimum is the only size left to agree on");
        assert_eq!(
            once,
            normalize_float_geom(once, h, wa, bw),
            "not a fixed point"
        );
        assert_eq!(
            snap_float_to_hints(once, h),
            once,
            "the answer must be the size the client's own correction asks for"
        );
        assert!(
            once.h + 2 * bw <= wa.h,
            "the answer stayed inside the workarea the frame was reserved from: {once:?}"
        );
        // The zero-extent width axis, clamped to the 1 px the protocol can
        // represent out of a workarea with no width at all.
        assert_eq!(once.w, 1, "a `ConfigureWindow` of 0 is BadValue");
    }

    /// The other way the same two stages could disagree: the 1 px protocol floor
    /// is not on the client's grid. A zero-extent request (the geometry a
    /// freshly managed client carries) for a client with no min and no max but
    /// an increment of 2 has no grid line at or below 1, so flooring the
    /// clamped 1 onto the grid is impossible and the 1 px floor has to be taken
    /// onto the grid instead — as 2, the line a `1` rounds to.
    ///
    /// Flooring *after* the increment round (the previous order) left the
    /// answer at 1, which the next call's snap then grew to 2: the float
    /// enlarged itself a whole increment after the first arrange.
    #[test]
    fn the_protocol_floor_lands_on_the_client_grid() {
        let h = SizeHints {
            base_w: 0,
            base_h: 0,
            inc_w: 2,
            inc_h: 2,
            valid: true,
            ..SizeHints::default()
        };
        let wa = Rect::new(0, 0, 200, 100);
        let once = normalize_float_geom(Rect::new(0, 0, 0, 0), h, wa, 0);
        assert_eq!((once.w, once.h), (2, 2), "the floor must land on the grid");
        assert_eq!(
            once,
            normalize_float_geom(once, h, wa, 0),
            "not a fixed point"
        );
        assert_eq!(snap_float_to_hints(once, h), once);
    }

    /// `float_client_authority` is the seal that ends the configure ping-pong:
    /// while it is set the WM adopted the client's own rect, so `arrange` must
    /// project that rect back with only protocol-level sanity applied — never
    /// re-normalizing it against the workarea and the hint grid, which is what
    /// makes a float "jump around by itself". Without the seal the WM is the
    /// authority again, and the same normalization applies; the placement also
    /// reports the *client's* border width, so a `Rule::border_w` override
    /// takes effect for floats.
    #[test]
    fn a_sealed_float_is_projected_back_verbatim() {
        proptest!(|(wa in screen_rect(), geom in screen_rect(), h in size_hints())| {
            let mut state = State::new();
            state.monitors.push(Monitor::new(wa, 2));
            state.monitors[0].workspaces[0].floats.push(7);
            let mut c = Client::new(7, 0, 0);
            c.geom = geom;
            c.hints = h;
            c.flags.set(WinFlags::FLOAT);
            c.border_w = 3;
            c.float_client_authority = true;
            state.add_client(c);

            let p = project(&state, &Cfg::default());
            prop_assert_eq!(p.len(), 1);
            prop_assert_eq!(p[0].0, 7);
            prop_assert_eq!(p[0].2, 3, "a float reports its own border width");
            prop_assert_eq!(
                p[0].1,
                adopt_client_float_geometry(geom),
                "a sealed float must be projected back as adopted, not re-normalized"
            );

            // Releasing the seal hands the authority back to the WM, which
            // normalizes the same rect against the workarea and the hints.
            state.clients.get_mut(&7).unwrap().float_client_authority = false;
            let q = project(&state, &Cfg::default());
            prop_assert_eq!(
                q[0].1,
                normalize_float_geom(geom, h, wa, 3),
                "an unsealed float must go through the WM's normalization"
            );
        });
    }
}

/// Cross-monitor float geometry.
///
/// `normalize_float_geom` is pure over `(geom, hints, workarea, border_w)` and
/// `clamp_float_geom` pins `g.x` into `[wa.x, wa.x + wa.w]`, so a float's
/// projection is bounded by the workarea it was handed and is blind to every
/// other monitor. The monitor-level contract that follows is what a genuine
/// cross-monitor bug would break, and it is deliberately stated on placements
/// rather than on the projection function so the tests are not the
/// implementation's own oracle:
///
/// * a float's placement for monitor `M` is inside `monitors[M].workarea`,
///   even when the geometry the model holds lies far outside it;
/// * that placement does not depend on any other monitor's screen, so no
///   neighbour's origin can drag a float across;
/// * moving a float to another monitor is a *state transition*: it re-settles
///   the client's own geometry into the destination workarea, so the record and
///   the placement agree afterwards.
#[cfg(test)]
mod cross_monitor_float {
    use crate::config::Cfg;
    use crate::core::commands::{Command, MoveWindowToMonitor};
    use crate::core::layout::{arrange, Placements, RibbonScratch};
    use crate::types::{Client, Dir, LayoutKind, Monitor, Rect, State, WinFlags, WindowId};
    use proptest::prelude::*;

    /// Run a full `arrange` for `mon_idx` and return the rects it produced.
    fn arrange_for(state: &State, mon_idx: usize) -> Placements {
        let mut out = Placements::default();
        arrange(
            state,
            mon_idx,
            &Cfg::default(),
            &mut out,
            &mut RibbonScratch::default(),
        );
        out
    }

    fn rect_of(placements: &Placements, win: WindowId) -> Option<Rect> {
        placements.iter().find(|p| p.0 == win).map(|p| p.1)
    }

    fn inside(r: Rect, wa: Rect) -> bool {
        r.x >= wa.x
            && r.y >= wa.y
            && r.x + r.w as i32 <= wa.x + wa.w as i32
            && r.y + r.h as i32 <= wa.y + wa.h as i32
    }

    /// Two side-by-side monitors with one float placed on the first.
    ///
    /// `geom` is what the model records, deliberately placed *outside* the
    /// first monitor's workarea so the clamp has real work to do.
    fn two_monitors_with_float(geom: Rect) -> State {
        let mut state = State::new();
        for screen in [Rect::new(0, 0, 1920, 1080), Rect::new(1920, 0, 1920, 1080)] {
            let mut mon = Monitor::new(screen, 1);
            mon.workspaces[0].layout = LayoutKind::Column;
            state.monitors.push(mon);
        }
        let mut c = Client::new(7, 0, 0);
        c.geom = geom;
        c.saved_geom = geom;
        c.flags.set(WinFlags::FLOAT);
        state.add_client(c);
        state.monitors[0].workspaces[0].floats.push(7);
        state.monitors[0].focus_stack.push(7);
        state.monitors[0].focused = Some(7);
        state.sel_mon = 0;
        state
    }

    /// A float whose recorded geometry lies entirely outside its own monitor is
    /// clamped into that monitor's workarea — and into *that* one, even though a
    /// second monitor sits immediately to its right with a reachable origin.
    ///
    /// This is the case that was previously misread as a cross-monitor
    /// re-anchoring: the coordinate moved to the workarea's left edge because
    /// the clamp bounds `g.x` by `wa.x`, not because another monitor was
    /// consulted. The monitor never changed here either.
    #[test]
    fn a_float_lands_in_its_own_monitor_workarea_not_a_neighbours() {
        // x/y sit inside the *right* monitor's workarea, which is the trap: the
        // float's own monitor must still win.
        let state = two_monitors_with_float(Rect::new(2000, 300, 300, 200));
        assert_eq!(
            state.clients[&7].monitor, 0,
            "the fixture must start on the left monitor"
        );
        let placed = rect_of(&arrange_for(&state, 0), 7).expect("the float is placed");
        let wa = state.monitors[0].workarea;
        assert!(
            inside(placed, wa),
            "a float on monitor 0 was placed outside its own workarea: {placed:?} not in {wa:?}"
        );
        assert!(
            state.monitors[1]
                .workspaces
                .iter()
                .all(|ws| ws.floats.is_empty()),
            "the float must not appear on the monitor it does not belong to"
        );
        assert!(
            rect_of(&arrange_for(&state, 1), 7).is_none(),
            "arranging monitor 1 must not produce a placement for monitor 0's float"
        );
    }

    /// A float that already fits its monitor keeps its position. This is the
    /// case that separates the clamp from a re-anchor: an implementation that
    /// pinned every float to the workarea origin would pass the
    /// out-of-workarea case above (the clamp lands on the origin there anyway)
    /// and fail only here, so both directions of the contract are pinned.
    #[test]
    fn a_float_inside_its_workarea_keeps_its_position() {
        let state = two_monitors_with_float(Rect::new(400, 300, 300, 200));
        let placed = rect_of(&arrange_for(&state, 0), 7).expect("the float is placed");
        let wa = state.monitors[0].workarea;
        assert!(
            inside(placed, wa),
            "a fitting float was placed outside its workarea: {placed:?} not in {wa:?}"
        );
        assert_ne!(
            placed.x, wa.x,
            "a float at x=400 was re-anchored to the workarea origin x={}",
            wa.x
        );
        assert_ne!(
            placed.y, wa.y,
            "a float at y=300 was re-anchored to the workarea origin y={}",
            wa.y
        );
    }

    /// The placement produced for one monitor does not depend on where the other
    /// monitor is. A neighbour's origin can therefore never drag a float across,
    /// which is the whole of the cross-monitor contract on the projection side.
    #[test]
    fn a_neighbouring_monitor_cannot_move_another_monitors_float() {
        // The float's own monitor is fixed at (0, 0, 1920, 1080) throughout; only
        // the neighbour varies — origin, size, a negative origin, and one far
        // away. The left monitor's placement must not move.
        let reference = {
            let base = two_monitors_with_float(Rect::new(2000, 300, 300, 200));
            rect_of(&arrange_for(&base, 0), 7).expect("placed")
        };
        for right in [
            Rect::new(1920, 0, 1920, 1080),
            Rect::new(0, 0, 640, 480),
            Rect::new(-2560, 300, 1280, 1024),
            Rect::new(11_000, -700, 400, 300),
        ] {
            let mut moved = two_monitors_with_float(Rect::new(2000, 300, 300, 200));
            moved.monitors[1] = Monitor::new(right, 1);
            moved.monitors[1].workspaces[0].layout = LayoutKind::Column;
            assert_eq!(
                rect_of(&arrange_for(&moved, 0), 7),
                Some(reference),
                "monitor 1's screen {right:?} changed monitor 0's float placement"
            );
        }
    }

    /// Moving a float to another monitor is a state transition, not a
    /// projection: the client's own record is re-settled into the destination
    /// workarea, so what the model records and what gets placed agree. The
    /// projection alone never performs this ownership change.
    #[test]
    fn moving_a_float_between_monitors_resettles_the_record() {
        let mut state = two_monitors_with_float(Rect::new(200, 200, 300, 200));
        let before = state.clients[&7].geom;
        assert_eq!(state.clients[&7].monitor, 0);

        let report = MoveWindowToMonitor(7, Dir::Right).execute(&mut state, &mut Cfg::default());
        assert!(
            !report.effects.is_empty(),
            "the move must be reported as work done"
        );
        let after = state.clients[&7].geom;
        assert_eq!(state.clients[&7].monitor, 1, "ownership follows the move");
        assert_ne!(
            before, after,
            "the destination workarea must re-settle the float"
        );
        let wa = state.monitors[1].workarea;
        assert!(
            inside(after, wa),
            "the re-settled rect {after:?} is outside the destination workarea {wa:?}"
        );
        assert_eq!(
            rect_of(&arrange_for(&state, 1), 7),
            Some(after),
            "the placement must match the record the transition wrote"
        );
        assert!(
            state.monitors[0]
                .workspaces
                .iter()
                .all(|ws| ws.floats.is_empty()),
            "the float must not stay on the monitor it left"
        );
    }

    fn arb_monitor_screen() -> impl Strategy<Value = Rect> {
        (-3840i32..7680, -2160i32..4320, 320u32..5120, 240u32..2880)
            .prop_map(|(x, y, w, h)| Rect::new(x, y, w, h))
    }

    fn arb_float_geom() -> impl Strategy<Value = Rect> {
        (-7680i32..15360, -4320i32..8640, 1u32..5120, 1u32..2880)
            .prop_map(|(x, y, w, h)| Rect::new(x, y, w, h))
    }

    // Across arbitrary monitor layouts — overlapping, negative origins,
    // different sizes and orders — a float's placement for its own monitor is
    // always inside that monitor's workarea, whatever geometry the record
    // holds. The workarea is the bound because that is what `clamp_float_geom`
    // clamps against; the point of the property is that no *other* monitor's
    // geometry can take part in the decision.
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn every_float_lands_inside_its_own_monitor_workarea(
            home in arb_monitor_screen(),
            other in arb_monitor_screen(),
            geom in arb_float_geom(),
        ) {
            let mut state = State::new();
            for screen in [home, other] {
                let mut mon = Monitor::new(screen, 1);
                mon.workspaces[0].layout = LayoutKind::Column;
                state.monitors.push(mon);
            }
            let mut c = Client::new(7, 0, 0);
            c.geom = geom;
            c.saved_geom = geom;
            c.flags.set(WinFlags::FLOAT);
            state.add_client(c);
            state.monitors[0].workspaces[0].floats.push(7);
            state.monitors[0].focus_stack.push(7);
            state.monitors[0].focused = Some(7);

            let placed = rect_of(&arrange_for(&state, 0), 7).expect("the float is placed");
            let wa = state.monitors[0].workarea;
            prop_assert!(
                inside(placed, wa),
                "float {:?} was placed at {:?}, outside monitor 0's workarea {:?} (other monitor {:?})",
                geom, placed, wa, other
            );
            prop_assert!(
                rect_of(&arrange_for(&state, 1), 7).is_none(),
                "monitor 1 produced a placement for monitor 0's float"
            );
        }
    }
}
