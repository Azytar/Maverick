// maverick/src/core/layout.rs
// Columnar layout engine (niri-style).
//
// Key idea: coordinates are COMPUTED, never stored.
// Column positions = f(scroll offset, column widths, gap).
// No mutable geom drift — every arrange() is a pure function over State.

use std::collections::HashMap;

use crate::config::Cfg;
use crate::types::{Client, LayoutKind, Monitor, Rect, State, ViewportMode, WindowId, Workspace};

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
/// they can never drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Settled,
    Live,
}

impl Phase {
    fn is_live(self) -> bool {
        matches!(self, Phase::Live)
    }
}

// ─── Layout trait ─────────────────────────────────────────────────────────────
//
// A `Layout` is a pluggable arrangement strategy. The core never matches on
// `LayoutKind` — it asks the registry for the layout's `arrange()` method.
// This means adding a new layout never touches `types.rs`, `engine.rs`, or
// `render.rs`: implement `Layout`, register it, and it works everywhere.
// (The old per-layout `handle_action` hook was never invoked — actions are
// mapped to typed `Command`s by `Engine::dispatch` instead.)

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

#[derive(Debug, Clone, Copy, Default)]
pub struct GridLayout;

impl Layout for GridLayout {
    fn name(&self) -> &'static str {
        "grid"
    }
    fn arrange(
        &self,
        state: &State,
        mon: &Monitor,
        cfg: &Cfg,
        _phase: Phase,
        out: &mut Placements,
        _scratch: &mut RibbonScratch,
    ) {
        let ws = mon.ws();
        let (placements, _snap) =
            crate::core::grid::arrange_workspace(ws, cfg, mon, ws.grid_snapshot.as_ref());
        let bw = cfg.border_w;
        for (win, rect) in placements {
            if state.clients.contains_key(&win) {
                out.push((win, rect, bw));
            }
        }
        // ── floating windows — keep existing geom, clamped to the full workarea ──
        for &win in &ws.floats {
            let Some(c) = state.clients.get(&win) else {
                continue;
            };
            let mut g = c.geom;
            g.x = g.x.clamp(
                mon.workarea.x,
                (mon.workarea.x + mon.workarea.w as i32)
                    .saturating_sub(g.w as i32)
                    .max(mon.workarea.x),
            );
            g.y = g.y.clamp(
                mon.workarea.y,
                (mon.workarea.y + mon.workarea.h as i32)
                    .saturating_sub(g.h as i32)
                    .max(mon.workarea.y),
            );
            g.w = g.w.min(mon.workarea.w);
            g.h = g.h.min(mon.workarea.h);
            out.push((win, g, c.border_w));
        }
    }
}

// ─── LayoutRegistry ───────────────────────────────────────────────────────────
//
// Maps `LayoutKind` → `Box<dyn Layout>`. Built once at startup from
// `compiled_config()`; external layouts can register themselves before the
// first arrange call.

pub struct LayoutRegistry {
    layouts: HashMap<LayoutKind, Box<dyn Layout>>,
}

impl LayoutRegistry {
    pub fn new() -> Self {
        let mut r = Self {
            layouts: HashMap::new(),
        };
        r.register(LayoutKind::Column, Box::new(ColumnLayout));
        r.register(LayoutKind::Grid, Box::new(GridLayout));
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

// NOTE: `arrange` computes ONLY the logical layout geometry (layout_rect).
// It is intentionally unaware of the *maximized* presentation overlay — that
// is applied afterwards by `core::present::present`. But fullscreen is
// special: in `LayoutKind::Column` it is a *normal participant of the
// scrolling ribbon* (niri-style), not an overlay. The fullscreen window
// becomes one column of the ribbon whose single tile measures `mon.screen`;
// it scrolls with the camera and leaves the screen when focus moves to a
// neighbour, instead of being a pinned always-on-top overlay. The only
// fullscreen overlay that remains is `LayoutKind::Grid`, where there is no
// scroll ribbon for it to join. This is driven entirely by `FsCtx` — a
// derived descriptor (never stored) passed through `ribbon_geom`,
// `arrange_columns`, `ideal_scroll` and `column_screen_extents` so all four
// agree on where the fullscreen column sits.

/// Count the number of tiled (non-floating) windows on a workspace.
fn count_tiled(ws: &Workspace) -> usize {
    ws.columns.iter().map(|c| c.windows.len()).sum()
}

/// A derived descriptor of the (single) fullscreen column on a workspace, if
/// any. It is NEVER stored in `State` — it is recomputed at every call site
/// that needs ribbon geometry, so `ribbon_geom`, `ideal_scroll` and
/// `column_screen_extents` can share one consistent view of where the
/// fullscreen tile lives without violating the borrow checker (those helpers
/// receive `&Workspace`, not `&State`).
///
/// The fullscreen window of a column is the FIRST window in that column with
/// the `FULLSCREEN` flag. Several columns may be fullscreen at once (niri-style:
/// each is a screen-filling ribbon column you scroll between with h/l). In
/// `LayoutKind::Grid` (no scroll ribbon to join) and in Overview (the
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

/// Resolve the effective inner/outer gaps for this workspace, applying
/// `smart_gaps` (collapse to 0 when exactly one tiled window).
fn effective_gaps(ws: &Workspace, cfg: &Cfg) -> (i32, i32) {
    if cfg.smart_gaps && count_tiled(ws) <= 1 && ws.floats.is_empty() {
        return (0, 0);
    }
    (cfg.gaps_inner as i32, cfg.gaps_outer as i32)
}

/// P10: Clear and refill `out` instead of allocating a new Vec each call.
//
// `phase` selects whether the window rests at its settled (target) geometry or
// is drawn at the live (current) geometry — see [`Phase`]. `arrange` keeps the
// historical default (live geometry: what you see today, no compositor).
pub fn arrange(
    state: &State,
    mon_idx: usize,
    cfg: &Cfg,
    registry: &LayoutRegistry,
    phase: Phase,
    out: &mut Placements,
    scratch: &mut RibbonScratch,
) {
    let mon = &state.monitors[mon_idx];
    let layout = registry.get(mon.ws().layout);
    // Always produce a fresh placement set. `out` is the WM's *shared* `desired`
    // buffer, which the compositor animation path also writes into
    // (see `compositor::live_placements`). Without this clear, the previous
    // frame's live placements leak in here, get re-applied by `apply_geom`,
    // and physically re-show windows that `hide_offscreen` just moved
    // off-screen (a fullscreen window on the now-inactive workspace would
    // reappear covering the new workspace).
    out.clear();
    layout.arrange(state, mon, cfg, phase, out, scratch);
}

// ─── Column layout ────────────────────────────────────────────────────────────
//
// Each column sits at a fixed x position (derived from sum of prior
// column widths + gaps). Windows within a column split vertically into
// uniformly-sized rows: focus never changes a window's geometry (no reflow
// on Up/Down navigation), it is marked with border/color only.

/// Single source of truth for the column-ribbon geometry. The renderer
/// (`arrange_columns`), the camera target (`ideal_scroll`) and
/// `column_screen_extents` all derive their numbers from this one function
/// so they can never drift apart again.
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
    let wa = Rect::new(
        workarea.x + gap_outer,
        workarea.y + gap_outer,
        workarea.w.saturating_sub((2 * gap_outer) as u32),
        workarea.h.saturating_sub((2 * gap_outer) as u32),
    );

    let alpha = (if settled { ws.zoom_target } else { ws.zoom }).max(0.05);
    // Viewport Zoom (Fase 9): when the workspace is in `Zoomed` mode the zoom
    // factor is `page_zoom` (which may be > 1 to *enlarge* the ribbon), not the
    // Overview `zoom`. They are kept separate on purpose — Overview zooms out
    // (`alpha < 1`), Viewport zooms in (`alpha > 1`). `ribbon_geom` already has
    // no upper clamp on `alpha`, so the enlargement falls out for free.
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
    // of how many columns exist* (bug C16): adding a column no longer shrinks
    // the others. The ribbon simply grows and the camera scrolls (niri-style).
    let usable_w = wa.w as f32;

    // Per-column accordion boost: the focused column eases toward 1.0 and the
    // others toward 0.0 (see `Workspace::tick_animations`), so changing focus
    // makes the widths *glide* instead of snapping (bug C10). In Overview the
    // boost is forced to 0 so every column sits at its base width and the strip
    // fits all of them.
    let total_boost = cfg.accordion_boost.clamp(0.0, 0.9);
    let focus_i = ws.focus.column_idx;

    cols.clear();
    let mut x: f32 = 0.0;
    for (i, c) in ws.columns.iter().enumerate() {
        let boost = if ws.overview {
            0.0
        } else if settled {
            // The settled boost is the *target* of the animation: the focused
            // column eases to 1.0 and every other column to 0.0, so the window
            // comes to rest at its final width — the camera has a fixed point to
            // ease toward (this is the "ideal_scroll" fix in the compositor plan:
            // reading the live boost made the camera target drift during the
            // accordion animation, which read as a residual slowness).
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
    let bw = cfg.border_w as i32;

    // Derived fullscreen descriptor — the single source of truth for where the
    // fullscreen tile lives in the ribbon. It is computed here (not stored) so
    // `ribbon_geom` and the camera target can share it without a `&State` borrow.
    let fs = fs_ctx(&state.clients, ws, mon.screen);

    // Single source of truth: the ribbon geometry for the requested phase.
    let g = scratch.ribbon_geom(ws, cfg, full_wa, phase.is_live(), &fs);
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
        // so bw is subtracted from the content width here.
        let inner_w = ((col_w_world * alpha) - 2.0 * bw as f32).max(1.0) as u32;
        // Only the (n-1) gaps *between* rows are reserved; top/bottom edges
        // sit flush with `wa`. Vertical also scales by `alpha` in Overview.
        let total_h = wa.h as f32 - (n as f32 - 1.0) * gap_f;

        // ── Row geometry (uniform rows) ───────────────────────────────────
        // The last row absorbs any remainder so the column always fills
        // `total_h` exactly; focus never resizes rows. Computed inline per row
        // rather than collected into a `Vec`, so the per-frame projection
        // allocates nothing.
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
            let row_y_world = wa.y as f32 + ri as f32 * (base_h + gap_f);
            let screen_h = (row_h_world * alpha).max(1.0) as u32;
            let screen_y = (wa.y as f32 + (row_y_world - wa.y as f32) * alpha + cy).round() as i32;

            let geom = Rect::new(
                screen_col_x,
                screen_y,
                inner_w,
                (screen_h as i32 - 2 * bw).max(1) as u32,
            );
            out.push((win, geom, cfg.border_w));
        }
    }

    // ── floating windows — keep existing geom, clamped to the full workarea ──
    for &win in &ws.floats {
        let client = match state.clients.get(&win) {
            Some(c) => c,
            None => continue,
        };
        let mut g = client.geom;
        // Clamp to workarea so the window is never completely off-screen.
        g.x = g.x.clamp(
            full_wa.x,
            (full_wa.x + full_wa.w as i32)
                .saturating_sub(g.w as i32)
                .max(full_wa.x),
        );
        g.y = g.y.clamp(
            full_wa.y,
            (full_wa.y + full_wa.h as i32)
                .saturating_sub(g.h as i32)
                .max(full_wa.y),
        );
        g.w = g.w.min(full_wa.w);
        g.h = g.h.min(full_wa.h);
        // Use the client's own border_w so Rule::border_w overrides take effect
        // for floating windows.
        out.push((win, g, client.border_w));
    }
}

// ─── Scroll helpers ───────────────────────────────────────────────────────────

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
    let g = ribbon_geom(ws, cfg, workarea, false, fs);
    g.cols
        .iter()
        .enumerate()
        .map(|(i, &(x, w))| {
            // Match `arrange_columns`' geometry exactly: the right edge is the
            // *inner* (border-exclusive) width, so the hit-test extent agrees
            // with the `client.geom` X11 hit-tests against (invariant A). A
            // fullscreen column is drawn with border 0, so it contributes no
            // border to subtract; tiled columns use `cfg.border_w`.
            let bw = if fs.cols.contains(&i) {
                0.0
            } else {
                cfg.border_w as f32
            };
            let l = g.wa.x as f32 + (x - ws.camera.position) * g.alpha + g.cx;
            let inner_w = (w * g.alpha - 2.0 * bw).max(1.0);
            (l, l + inner_w)
        })
        .collect()
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

    // ─── Regression: the three ribbon functions must agree for a fullscreen
    // column (plan invariant: `ribbon_geom` is the single source of truth). ───

    /// Two columns, col 0 fullscreen, focused on col 0. The placement's left
    /// edge, `column_screen_extents`' left edge and the centered/aligned camera
    /// must all be consistent.
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
}
