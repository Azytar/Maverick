// maverick/src/core/layout.rs
// Columnar layout engine (niri-style).
//
// Key idea: coordinates are COMPUTED, never stored.
// Column positions = f(scroll offset, column widths, gap).
// No mutable geom drift — every arrange() is a pure function over State.

use crate::config::Cfg;
use crate::types::{LayoutKind, Monitor, Rect, State, WindowId, Workspace};

pub type Placements = Vec<(WindowId, Rect, u32)>; // (win, geom, border_w)

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
    fn arrange(&self, state: &State, mon: &Monitor, cfg: &Cfg, out: &mut Placements);
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ColumnLayout;

impl Layout for ColumnLayout {
    fn name(&self) -> &'static str {
        "column"
    }
    fn arrange(&self, state: &State, mon: &Monitor, cfg: &Cfg, out: &mut Placements) {
        arrange_columns(state, mon, cfg, out);
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GridLayout;

impl Layout for GridLayout {
    fn name(&self) -> &'static str {
        "grid"
    }
    fn arrange(&self, state: &State, mon: &Monitor, cfg: &Cfg, out: &mut Placements) {
        arrange_grid(state, mon, cfg, out);
    }
}

// ─── LayoutRegistry ───────────────────────────────────────────────────────────
//
// Maps `LayoutKind` → `Box<dyn Layout>`. Built once at startup from
// `compiled_config()`; external layouts can register themselves before the
// first arrange call.

use std::collections::HashMap;

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
// It is intentionally unaware of presentation modes such as fullscreen —
// that is applied afterwards by `core::present::present`, which projects the
// presentation overlay (fullscreen/maximized) on top. Keeping layout pure
// means focus can move freely without the layout and presentation desyncing,
// and the overlay's geometry never depends on focus.

/// Count the number of tiled (non-floating) windows on a workspace.
fn count_tiled(ws: &Workspace) -> usize {
    ws.columns.iter().map(|c| c.windows.len()).sum()
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
pub fn arrange(
    state: &State,
    mon_idx: usize,
    cfg: &Cfg,
    registry: &LayoutRegistry,
    out: &mut Placements,
) {
    let mon = &state.monitors[mon_idx];
    let layout = registry.get(mon.ws().layout);
    layout.arrange(state, mon, cfg, out);
}

// ─── Column layout ────────────────────────────────────────────────────────────
//
// Each column sits at a fixed x position (derived from sum of prior
// column widths + gaps). Windows within a column split vertically into
// uniformly-sized rows: focus never changes a window's geometry (no reflow
// on Up/Down navigation), it is marked with border/color only.

fn arrange_columns(state: &State, mon: &Monitor, cfg: &Cfg, out: &mut Placements) {
    let ws = mon.ws();
    let wa = mon.workarea;
    let (gap, _gap_outer) = effective_gaps(ws, cfg);
    let bw = cfg.border_w as i32;

    // ── tiled windows ──
    let mut col_x = wa.x - ws.scroll;
    for col in &ws.columns {
        let col_w = col.width as i32;
        let n = col.windows.len();
        if n == 0 {
            col_x += col_w + gap;
            continue;
        }

        let inner_w = (col_w - 2 * bw - 2 * gap).max(1);
        let total_h = wa.h as i32 - 2 * gap;
        let base_h = if n > 1 { total_h / n as i32 } else { total_h };

        // ── Precompute (row_h, row_y) for every row in O(N) ──────────────
        // Uniform rows: the last row absorbs any remainder so the column
        // always fills `total_h` exactly. Focus never resizes rows.
        let row_info: Vec<(i32, i32)> = if n == 1 {
            vec![(total_h.max(1), wa.y + gap)]
        } else {
            (0..n)
                .map(|i| {
                    let extra = if i == n - 1 {
                        total_h - base_h * n as i32
                    } else {
                        0
                    };
                    let h = (base_h + extra - gap).max(1);
                    let y = wa.y + gap + i as i32 * (base_h + gap);
                    (h, y)
                })
                .collect()
        };

        for (ri, &win) in col.windows.iter().enumerate() {
            if !state.clients.contains_key(&win) {
                continue;
            }

            let (row_h, row_y) = row_info[ri];

            let geom = Rect::new(
                col_x + gap + bw,
                row_y + bw,
                // inner_w = col_w - 2*bw - 2*gap already accounts for both borders.
                inner_w.max(1) as u32,
                (row_h - 2 * bw).max(1) as u32,
            );
            out.push((win, geom, cfg.border_w));
        }

        col_x += col_w + gap;
    }

    // ── floating windows — keep existing geom, clamped to workarea ──
    for &win in &ws.floats {
        let client = match state.clients.get(&win) {
            Some(c) => c,
            None => continue,
        };
        let mut g = client.geom;
        // Clamp to workarea so the window is never completely off-screen.
                g.x = g.x.clamp(
            wa.x,
            (wa.x + wa.w as i32).saturating_sub(g.w as i32).max(wa.x),
        );
        g.y = g.y.clamp(
            wa.y,
            (wa.y + wa.h as i32).saturating_sub(g.h as i32).max(wa.y),
        );
        g.w = g.w.min(wa.w);
        g.h = g.h.min(wa.h);
        // Use the client's own border_w so Rule::border_w overrides take effect
        // for floating windows.
        out.push((win, g, client.border_w));
    }
}

fn arrange_grid(state: &State, mon: &Monitor, cfg: &Cfg, out: &mut Placements) {
    let ws = mon.ws();
    let wa = mon.workarea;
    let (gap, _gap_outer) = effective_gaps(ws, cfg);
    let bw = cfg.border_w as i32;

    let wins: Vec<WindowId> = ws
        .columns
        .iter()
        .flat_map(|c| c.windows.iter().copied())
        .collect();
    let n = wins.len();
    if n == 0 {
        return;
    }

    let cols = (n as f64).sqrt().ceil() as usize;
    let rows = n.div_ceil(cols);
    let cell_w = (wa.w as i32 - gap * (cols as i32 + 1)) / cols as i32;
    let cell_h = (wa.h as i32 - gap * (rows as i32 + 1)) / rows as i32;

    for (i, &win) in wins.iter().enumerate() {
        if !state.clients.contains_key(&win) {
            continue;
        }
        let col = i % cols;
        let row = i / cols;
        let geom = Rect::new(
            wa.x + gap + col as i32 * (cell_w + gap) + bw,
            wa.y + gap + row as i32 * (cell_h + gap) + bw,
            (cell_w - 2 * bw).max(1) as u32,
            (cell_h - 2 * bw).max(1) as u32,
        );
        out.push((win, geom, cfg.border_w));
    }

        for &win in &ws.floats {
        if let Some(c) = state.clients.get(&win) {
            // Use the client's own border_w so Rule::border_w overrides take effect.
            out.push((win, c.geom, c.border_w));
        }
    }
}

// ─── Scroll helpers ───────────────────────────────────────────────────────────

/// Compute the ideal scroll so the focused column is fully visible (niri-style centering).
pub fn ideal_scroll(mon: &Monitor, cfg: &Cfg) -> i32 {
    let ws = mon.ws();
    if ws.columns.is_empty() {
        return 0;
    }

    // Guard: column_idx can be stale if cleanup_empty_columns hasn't run yet
    let col_idx = ws.focus.column_idx.min(ws.columns.len().saturating_sub(1));

            let gap = cfg.gaps_inner as i32;
    let wa_w = mon.workarea.w as i32;

    // x of focused column (relative to virtual origin).
    // Use saturating_add to avoid overflow with many columns.
    let col_x_virtual: i32 = ws.columns[..col_idx]
        .iter()
        .map(|c| (c.width as i32).saturating_add(gap))
        .fold(0i32, i32::saturating_add);

    let focused_w = ws.columns[col_idx].width as i32;
    let focused_center = col_x_virtual.saturating_add(focused_w / 2);
    let screen_center = wa_w / 2;

    (focused_center.saturating_sub(screen_center)).max(0)
}
