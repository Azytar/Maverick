// maverick/src/core/grid.rs
// Pure `Grid` layout engine — no X11, no `State`, no focus, no event loop.
//
// The geometry is a pure function of (window set, workarea, gaps, border). The
// only piece of state it *reads* is an optional previous `GridSnapshot`, used
// purely to keep existing windows from jumping around when the window count
// changes. The engine is fully deterministic: candidate partitions are
// enumerated in a fixed order, the cost/candidate path never iterates a
// `HashMap`, and all tie-breaks are explicit.

use crate::config::Cfg;
use crate::types::{Dir, GridPlacement, GridSnapshot, Monitor, Rect, WindowId, Workspace};

// ─── Cost weights ─────────────────────────────────────────────────────────────
//
// The aspect term is order ~1 per cell; the movement term is measured in
// pixels. `W_STABLE` is kept small so geometry (aspect/balance) dominates the
// choice of partition, while still preferring to keep an existing window where
// it was when two partitions are otherwise comparable. `W_BALANCE` is
// negligible because every cell in a candidate is (near-)uniformly sized, so
// cell-area variance is already ~0.
const W_STABLE: f64 = 0.0015;
const W_AREA: f64 = 1e-6;
const W_BALANCE: f64 = 1e-9;

/// Enumerate balanced row-partitions of `n` windows, with longer rows on top.
///
/// For each row count `R` in `1..=n` the rows carry `base = n/R` windows, with
/// the first `rem = n%R` rows getting one extra (`base+1`). Skips any `R` that
/// would leave a row empty (`base == 0`, which cannot happen for `R <= n`).
/// This yields both the rigid (`[k,k]` for `n = k*k`) and non-rigid
/// (`[3,2]` for `n=5`, `[3,2,2]` for `n=7`) layouts.
fn partitions(n: usize) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    if n == 0 {
        return out;
    }
    for r in 1..=n {
        let base = n / r;
        if base == 0 {
            continue;
        }
        let rem = n % r;
        let mut rows = Vec::with_capacity(r);
        for _ in 0..rem {
            rows.push(base + 1);
        }
        for _ in 0..(r - rem) {
            rows.push(base);
        }
        out.push(rows);
    }
    out
}

/// Per-column widths and per-row heights for a partition, with the leftover
/// pixels (from integer division) redistributed 1px at a time to the *left*
/// columns and *top* rows — matching the project's column/row convention so no
/// 1px gap or overflow is left at the right/bottom edge.
fn cell_geom(rows: &[usize], area: Rect, gap: i32) -> (Vec<i32>, Vec<i32>) {
    let m = (*rows.iter().max().unwrap_or(&1)) as i32;
    let r = rows.len() as i32;
    let cell_w = (area.w as i32 - (m - 1) * gap).max(m) / m.max(1);
    let cell_h = (area.h as i32 - (r - 1) * gap).max(r) / r.max(1);
    let mut col_w = vec![cell_w; m as usize];
    let mut row_h = vec![cell_h; r as usize];

    // Leftover px (the division remainder) go to the leftmost columns / top rows.
    let lw = area.w as i32 - (m * cell_w + (m - 1) * gap);
    for c in 0..lw.max(0) as usize {
        if c < col_w.len() {
            col_w[c] += 1;
        }
    }
    let lh = area.h as i32 - (r * cell_h + (r - 1) * gap);
    for rr in 0..lh.max(0) as usize {
        if rr < row_h.len() {
            row_h[rr] += 1;
        }
    }
    (col_w, row_h)
}

/// Build the cells (row-major, left→right, skipping the slots of short bottom
/// rows) as content rects (border already subtracted from width/height, x/y are
/// border-inclusive top-left, X11 semantics).
fn build_cells(rows: &[usize], area: Rect, gap: i32, border: i32) -> Vec<(usize, usize, Rect)> {
    let (col_w, row_h) = cell_geom(rows, area, gap);
    let mut cells = Vec::new();
    let mut y = area.y;
    for (ri, &len) in rows.iter().enumerate() {
        let mut x = area.x;
        for (ci, &cw) in col_w.iter().enumerate().take(len) {
            let ch = row_h[ri];
            let w = (cw - 2 * border).max(1) as u32;
            let h = (ch - 2 * border).max(1) as u32;
            cells.push((ri, ci, Rect::new(x, y, w, h)));
            x += cw + gap;
        }
        y += row_h[ri] + gap;
    }
    cells
}

#[inline]
fn center_x(r: Rect) -> i32 {
    r.x + r.w as i32 / 2
}
#[inline]
fn center_y(r: Rect) -> i32 {
    r.y + r.h as i32 / 2
}

/// Assign windows to cells, keeping the layout *stable* across re-arrangements:
///
/// * A window that already existed in `prev` keeps its previous `(row, col)`
///   slot whenever that slot still exists in the new partition — so adding or
///   removing a window leaves every survivor exactly where it was.
/// * A `prev` window whose old slot no longer exists (the partition changed
///   shape) is anchored to the free cell whose previous center is closest.
/// * New windows fill whatever cells are left, in stable `wins` order.
///
/// Returns the placements plus the sum of movement (|`Δcenter_x`| + |`Δcenter_y`| +
/// `W_AREA`·|Δarea|) over windows that existed in `prev`.
fn assign_windows(
    wins: &[WindowId],
    cells: &[(usize, usize, Rect)],
    prev: Option<&GridSnapshot>,
) -> (Vec<(WindowId, Rect)>, GridSnapshot, f64) {
    let n = cells.len();
    let mut cell_win: Vec<Option<WindowId>> = vec![None; n];

    // Previous (row, col) and geometry of a window, if it was tiled before.
    let prev_info = |win: WindowId| -> Option<(usize, usize, i64, i64, f64)> {
        prev.and_then(|s| {
            s.placements.iter().find(|p| p.win == win).map(|p| {
                (
                    p.row,
                    p.col,
                    center_x(p.rect) as i64,
                    center_y(p.rect) as i64,
                    p.rect.w as f64 * p.rect.h as f64,
                )
            })
        })
    };
    let cell_index = |r: usize, c: usize| -> Option<usize> {
        cells.iter().position(|&(rr, cc, _)| rr == r && cc == c)
    };

    let mut remaining: Vec<WindowId> = wins.to_vec();
    let mut deferred: Vec<WindowId> = Vec::new();

    // Pass 1: keep the same (row, col) when the slot still exists.
    for &win in wins {
        if let Some((pr, pc, _, _, _)) = prev_info(win) {
            if let Some(i) = cell_index(pr, pc) {
                if cell_win[i].is_none() {
                    cell_win[i] = Some(win);
                    if let Some(pos) = remaining.iter().position(|&w| w == win) {
                        remaining.remove(pos);
                    }
                    continue;
                }
            }
            deferred.push(win);
        }
    }

    // Pass 2: anchor displaced previous windows to their nearest free cell.
    for &win in &deferred {
        if let Some((_, _, pcx, pcy, _)) = prev_info(win) {
            let mut best_i: Option<usize> = None;
            let mut best_d: i64 = i64::MAX;
            for (i, &(_, _, rect)) in cells.iter().enumerate() {
                if cell_win[i].is_some() {
                    continue;
                }
                let dx = center_x(rect) as i64 - pcx;
                let dy = center_y(rect) as i64 - pcy;
                let d = dx * dx + dy * dy;
                if d < best_d {
                    best_d = d;
                    best_i = Some(i);
                }
            }
            if let Some(i) = best_i {
                cell_win[i] = Some(win);
                if let Some(pos) = remaining.iter().position(|&w| w == win) {
                    remaining.remove(pos);
                }
            }
        }
    }

    // Pass 3: fill the leftover cells with any remaining (new) windows.
    let mut wi = 0;
    for slot in cell_win.iter_mut().take(n) {
        if slot.is_none() {
            *slot = Some(remaining[wi]);
            wi += 1;
        }
    }

    // Movement: only windows that existed in `prev` contribute.
    let mut movement = 0.0_f64;
    for i in 0..n {
        let win = cell_win[i].expect("every cell is assigned");
        if let Some((_, _, pcx, pcy, parea)) = prev_info(win) {
            let rect = cells[i].2;
            let dx = (center_x(rect) as f64 - pcx as f64).abs();
            let dy = (center_y(rect) as f64 - pcy as f64).abs();
            let da = (rect.w as f64 * rect.h as f64 - parea).abs();
            movement += dx + dy + W_AREA * da;
        }
    }

    let mut placements = Vec::with_capacity(n);
    let mut snapshot = GridSnapshot {
        placements: Vec::with_capacity(n),
    };
    for i in 0..n {
        let win = cell_win[i].expect("every cell is assigned");
        let (r, c, rect) = cells[i];
        placements.push((win, rect));
        snapshot.placements.push(GridPlacement {
            win,
            rect,
            row: r,
            col: c,
        });
    }
    (placements, snapshot, movement)
}

/// Pure grid arrangement.
///
/// * `wins`   — stable window order (the flat `ws.columns` order).
/// * `area`   — the workarea *already inset* by the outer gap.
/// * `gap`    — inner gap (px) between cells.
/// * `border` — per-window border (px) subtracted from content width/height.
/// * `prev`   — previous frame's snapshot, for stability (may be `None`).
///
/// Returns `(placements, snapshot)` where `placements` pairs each window with
/// its base (pre-fullscreen-overlay) content rect.
pub fn arrange(
    wins: &[WindowId],
    area: Rect,
    gap: i32,
    border: i32,
    prev: Option<&GridSnapshot>,
) -> (Vec<(WindowId, Rect)>, GridSnapshot) {
    let n = wins.len();
    if n == 0 {
        return (Vec::new(), GridSnapshot::default());
    }

    let parts = partitions(n);
    let aspect_target = if area.h > 0 {
        area.w as f64 / area.h as f64
    } else {
        1.0
    };
    let eps = 1e-9_f64;

    type Best = Option<(Vec<usize>, f64, Vec<(WindowId, Rect)>, GridSnapshot)>;
    let mut best: Best = None;

    for rows in &parts {
        let cells = build_cells(rows, area, gap, border);
        let aspect_cost: f64 = cells
            .iter()
            .map(|&(_, _, r)| {
                let a = if r.h > 0 {
                    r.w as f64 / r.h as f64
                } else {
                    1.0
                };
                (a - aspect_target) * (a - aspect_target)
            })
            .sum();
        let areas: Vec<f64> = cells
            .iter()
            .map(|&(_, _, r)| r.w as f64 * r.h as f64)
            .collect();
        let mean = areas.iter().sum::<f64>() / areas.len() as f64;
        let var = areas.iter().map(|a| (a - mean) * (a - mean)).sum::<f64>() / areas.len() as f64;

        let (placements, snapshot, movement) = assign_windows(wins, &cells, prev);
        let cost = aspect_cost + W_BALANCE * var + W_STABLE * movement;

        let take = match &best {
            None => true,
            Some(b) => {
                if cost < b.1 - eps {
                    true
                } else if (cost - b.1).abs() <= eps {
                    // Exact cost tie: prefer fewer rows, then lexicographically
                    // smaller row-vector (deterministic, no HashMap).
                    rows.len() < b.0.len() || (rows.len() == b.0.len() && *rows < b.0)
                } else {
                    false
                }
            }
        };
        if take {
            best = Some((rows.clone(), cost, placements, snapshot));
        }
    }

    let best = best.expect("n > 0 implies at least one partition");
    (best.2, best.3)
}

/// Spatial neighbour of `focused` among `placements` in direction `dir`.
///
/// Candidates are strictly on the `dir` side of the focused window's center;
/// among them the primary axis distance selects, with the secondary axis
/// distance as tie-break. Works for rows of differing length (no `i±1`
/// indexing).
pub fn neighbor(placements: &[(WindowId, Rect)], focused: WindowId, dir: Dir) -> Option<WindowId> {
    neighbor_dir(placements, focused, dir)
}

/// Re-derive the grid geometry for a real workspace (handles the outer-gap
/// inset, the flat window order, and stability via the stored snapshot). Shared
/// by `GridLayout::arrange`, the render-path snapshot capture, and the spatial
/// focus/move helpers so there is exactly one source of grid truth.
pub fn arrange_workspace(
    ws: &Workspace,
    cfg: &Cfg,
    mon: &Monitor,
    prev: Option<&GridSnapshot>,
) -> (Vec<(WindowId, Rect)>, GridSnapshot) {
    let (gap, gap_outer) = effective_gaps(ws, cfg);
    let area = Rect::new(
        mon.workarea.x + gap_outer,
        mon.workarea.y + gap_outer,
        mon.workarea.w.saturating_sub((2 * gap_outer) as u32),
        mon.workarea.h.saturating_sub((2 * gap_outer) as u32),
    );
    let wins: Vec<WindowId> = ws
        .columns
        .iter()
        .flat_map(|c| c.windows.iter().copied())
        .collect();
    arrange(&wins, area, gap, cfg.border_w as i32, prev)
}

fn count_tiled(ws: &Workspace) -> usize {
    ws.columns.iter().map(|c| c.windows.len()).sum()
}

fn effective_gaps(ws: &Workspace, cfg: &Cfg) -> (i32, i32) {
    if cfg.smart_gaps && count_tiled(ws) <= 1 && ws.floats.is_empty() {
        return (0, 0);
    }
    (cfg.gaps_inner as i32, cfg.gaps_outer as i32)
}

/// Spatial neighbour body shared by the public `neighbor` (operates on
/// `crate::types::Dir`). Candidates are strictly on the `dir` side of the
/// focused window's center; among them the primary axis distance selects, with
/// the secondary axis distance as tie-break.
fn neighbor_dir(placements: &[(WindowId, Rect)], focused: WindowId, dir: Dir) -> Option<WindowId> {
    let f = placements
        .iter()
        .find(|(w, _)| *w == focused)
        .map(|(_, r)| *r)?;
    let fx = center_x(f) as i64;
    let fy = center_y(f) as i64;

    let mut best: Option<(WindowId, i64, i64)> = None; // (win, primary, secondary)
    for &(w, r) in placements {
        if w == focused {
            continue;
        }
        let cx = center_x(r) as i64;
        let cy = center_y(r) as i64;
        let (primary, secondary) = match dir {
            Dir::Right => {
                if cx > fx {
                    (cx - fx, (cy - fy).abs())
                } else {
                    continue;
                }
            }
            Dir::Left => {
                if cx < fx {
                    (fx - cx, (cy - fy).abs())
                } else {
                    continue;
                }
            }
            Dir::Up => {
                if cy < fy {
                    (fy - cy, (cx - fx).abs())
                } else {
                    continue;
                }
            }
            Dir::Down => {
                if cy > fy {
                    (cy - fy, (cx - fx).abs())
                } else {
                    continue;
                }
            }
            _ => continue,
        };
        let take = match best {
            None => true,
            Some((_, bp, bs)) => primary < bp || (primary == bp && secondary < bs),
        };
        if take {
            best = Some((w, primary, secondary));
        }
    }
    best.map(|(w, _, _)| w)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Cfg;

    fn cfg() -> Cfg {
        Cfg {
            gaps_inner: 6,
            gaps_outer: 6,
            border_w: 2,
            smart_gaps: false,
            ..Default::default()
        }
    }

    /// Workarea after the outer-gap inset, matching what `arrange_workspace`
    /// passes to `arrange`.
    fn area() -> Rect {
        let full = Rect::new(0, 0, 1920, 1080);
        let go = cfg().gaps_outer as i32;
        Rect::new(
            full.x + go,
            full.y + go,
            full.w - (2 * go as u32),
            full.h - (2 * go as u32),
        )
    }

    fn wins(n: usize) -> Vec<WindowId> {
        (1..=n as u32).collect()
    }

    fn border() -> i32 {
        cfg().border_w as i32
    }
    fn gap() -> i32 {
        cfg().gaps_inner as i32
    }

    fn run(n: usize) -> (Vec<(WindowId, Rect)>, GridSnapshot) {
        arrange(&wins(n), area(), gap(), border(), None)
    }

    #[test]
    fn grid_1_window_fills_workarea() {
        let (p, s) = run(1);
        assert_eq!(p.len(), 1);
        assert_eq!(s.placements.len(), 1);
        let (_, r) = p[0];
        assert_eq!(r.x, area().x);
        assert_eq!(r.y, area().y);
        assert!(r.w as i32 + 2 * border() >= area().w as i32 - 2);
    }

    #[test]
    fn rects_do_not_overlap() {
        for n in 1..=10usize {
            let (p, _) = run(n);
            for i in 0..p.len() {
                for j in (i + 1)..p.len() {
                    let a = p[i].1;
                    let b = p[j].1;
                    let overlap = a.x < b.x + b.w as i32
                        && b.x < a.x + a.w as i32
                        && a.y < b.y + b.h as i32
                        && b.y < a.y + a.h as i32;
                    assert!(!overlap, "n={n}: cells {i} and {j} overlap");
                }
            }
        }
    }

    #[test]
    fn rects_stay_inside_workarea() {
        let a = area();
        for n in 1..=10usize {
            let (p, _) = run(n);
            for (w, r) in &p {
                let _ = w;
                assert!(r.x >= a.x, "n={n}: x {} < {}", r.x, a.x);
                assert!(r.y >= a.y);
                assert!(
                    r.x + r.w as i32 <= a.x + a.w as i32,
                    "n={n}: right edge overflows"
                );
                assert!(r.y + r.h as i32 <= a.y + a.h as i32);
            }
        }
    }

    #[test]
    fn gaps_are_exact() {
        // The top row is always full (longest rows on top) and the left column
        // is always full, so checking those two edges proves the cells tile the
        // area exactly (modulo the per-window border that sits inside each cell
        // frame). Adjacent frames are exactly `gap + 2*border` apart.
        let a = area();
        let g = gap();
        let b = border();
        for n in 1..=10usize {
            let (p, _s) = run(n);
            // top row: cells whose y == min y.
            let min_y = p.iter().map(|(_, r)| r.y).min().unwrap();
            let mut top: Vec<_> = p
                .iter()
                .filter(|(_, r)| r.y == min_y)
                .map(|(w, r)| (*w, *r))
                .collect();
            top.sort_by_key(|(_, r)| r.x);
            assert_eq!(
                top[0].1.x, a.x,
                "n={n}: top row does not start at the left edge"
            );
            for pair in top.windows(2) {
                let expected = pair[0].1.x + pair[0].1.w as i32 + 2 * b + g;
                assert_eq!(
                    pair[1].1.x, expected,
                    "n={n}: horizontal gap between top-row cells is off"
                );
            }
            let last = top.last().unwrap().1;
            assert_eq!(
                last.x + last.w as i32 + 2 * b,
                a.x + a.w as i32,
                "n={n}: top row does not reach the right edge"
            );

            // left column: cells whose x == min x.
            let min_x = p.iter().map(|(_, r)| r.x).min().unwrap();
            let mut left: Vec<_> = p
                .iter()
                .filter(|(_, r)| r.x == min_x)
                .map(|(w, r)| (*w, *r))
                .collect();
            left.sort_by_key(|(_, r)| r.y);
            assert_eq!(left[0].1.y, a.y, "n={n}: left column starts at top");
            for pair in left.windows(2) {
                let expected = pair[0].1.y + pair[0].1.h as i32 + 2 * b + g;
                assert_eq!(
                    pair[1].1.y, expected,
                    "n={n}: vertical gap between left-column cells is off"
                );
            }
            let last = left.last().unwrap().1;
            assert_eq!(
                last.y + last.h as i32 + 2 * b,
                a.y + a.h as i32,
                "n={n}: left column does not reach the bottom edge"
            );
        }
    }

    #[test]
    fn layout_is_deterministic() {
        for n in 1..=8usize {
            let a = run(n);
            let b = run(n);
            assert_eq!(a.0, b.0, "n={n}: placements not deterministic");
            assert_eq!(
                a.1.placements, b.1.placements,
                "n={n}: snapshot not deterministic"
            );
        }
    }

    #[test]
    fn non_rigid_preferred() {
        // n=5 should pick [3,2] (rows of differing length), not a forced rigid
        // [5] or [1,1,1,1,1] grid.
        let (_, s) = run(5);
        // Recover the partition row-lengths from the snapshot.
        let max_row = s.placements.iter().map(|p| p.row).max().unwrap();
        let mut row_len = vec![0usize; max_row + 1];
        for p in &s.placements {
            row_len[p.row] += 1;
        }
        let distinct = row_len.iter().filter(|&&l| l > 0).collect::<Vec<_>>();
        assert!(
            distinct.windows(2).any(|w| w[0] != w[1]),
            "n=5 must choose a non-rigid partition, got {row_len:?}"
        );
        assert_eq!(s.placements.len(), 5);
    }

    #[test]
    fn aspect_aware_cell_orientation() {
        // The partition is monitor-aspect independent, but the *cell dimensions*
        // must follow the monitor: a wide monitor yields wide cells, a tall
        // (portrait) monitor yields tall cells, for the same window count.
        let g = gap();
        let b = border();
        let wide = Rect::new(0, 0, 2560, 1080);
        let tall = Rect::new(0, 0, 1080, 2560);
        let (pw, _) = arrange(&wins(5), wide, g, b, None);
        let (pt, _) = arrange(&wins(5), tall, g, b, None);
        let wide_cell = pw[0].1;
        let tall_cell = pt[0].1;
        assert!(
            wide_cell.w as i32 > wide_cell.h as i32,
            "wide monitor must produce wide grid cells"
        );
        assert!(
            tall_cell.h as i32 > tall_cell.w as i32,
            "tall monitor must produce tall grid cells"
        );
    }

    #[test]
    fn directional_navigation() {
        // 6 windows → 2x3 (R=3 rows, M=2 cols) on a 16:9 area. Build the grid
        // and navigate geometrically.
        let (p, _) = run(6);
        // Find the top-left window (min x, min y), then walk right/down.
        let top_left = p.iter().min_by_key(|(_, r)| (r.y, r.x)).unwrap().0;
        let right = neighbor(&p, top_left, crate::types::Dir::Right).unwrap();
        let tl = p.iter().find(|(w, _)| *w == top_left).unwrap().1;
        let rg = p.iter().find(|(w, _)| *w == right).unwrap().1;
        assert!(rg.x > tl.x, "Right neighbour must be to the right");
        assert_eq!(rg.y, tl.y, "Right neighbour stays on the same row");

        let down = neighbor(&p, top_left, crate::types::Dir::Down).unwrap();
        let dg = p.iter().find(|(w, _)| *w == down).unwrap().1;
        assert!(dg.y > tl.y, "Down neighbour must be below");
        assert_eq!(dg.x, tl.x, "Down neighbour stays in the same column");

        // Top-left has no left/up neighbour.
        assert_eq!(neighbor(&p, top_left, crate::types::Dir::Left), None);
        assert_eq!(neighbor(&p, top_left, crate::types::Dir::Up), None);
    }

    #[test]
    fn directional_navigation_short_row() {
        // n=5 → [3,2]: bottom row has only 2 cells (cols 0,1); col 2 of the
        // bottom row is empty, so the bottom-right cell's "Down" must be None
        // (no cell directly below it).
        let (p, _) = run(5);
        // bottom row = max y.
        let max_y = p.iter().map(|(_, r)| r.y).max().unwrap();
        let bottom: Vec<_> = p.iter().filter(|(_, r)| r.y == max_y).collect();
        // The bottom-right-most cell:
        let br = bottom.iter().max_by_key(|(_, r)| r.x).unwrap();
        let nb = neighbor(&p, br.0, crate::types::Dir::Down);
        assert_eq!(nb, None, "no cell directly below the short bottom row");
        // But it can still navigate left/up.
        assert!(neighbor(&p, br.0, crate::types::Dir::Left).is_some());
        assert!(neighbor(&p, br.0, crate::types::Dir::Up).is_some());
    }

    #[test]
    fn insertion_preserves_relative_order() {
        // Start from a 2x2 of {A,B,C,D}. Add E. The four originals must keep
        // their pairwise left/right and above/below relationships.
        let (p0, s0) = arrange(&wins(4), area(), gap(), border(), None);
        let snap = Some(s0);
        let (p1, _s1) = arrange(&wins(5), area(), gap(), border(), snap.as_ref());

        let rel_before = |a: WindowId, b: WindowId| -> (std::cmp::Ordering, std::cmp::Ordering) {
            let ra = p0.iter().find(|(w, _)| *w == a).unwrap().1;
            let rb = p0.iter().find(|(w, _)| *w == b).unwrap().1;
            (
                center_x(ra).cmp(&center_x(rb)),
                center_y(ra).cmp(&center_y(rb)),
            )
        };
        let rel_after = |a: WindowId, b: WindowId| -> (std::cmp::Ordering, std::cmp::Ordering) {
            let ra = p1.iter().find(|(w, _)| *w == a).unwrap().1;
            let rb = p1.iter().find(|(w, _)| *w == b).unwrap().1;
            (
                center_x(ra).cmp(&center_x(rb)),
                center_y(ra).cmp(&center_y(rb)),
            )
        };
        for a in 1..=4u32 {
            for b in 1..=4u32 {
                if a == b {
                    continue;
                }
                assert_eq!(
                    rel_before(a, b),
                    rel_after(a, b),
                    "relative order of {a}/{b} changed on insertion of E"
                );
            }
        }
    }

    #[test]
    fn removal_preserves_relative_order() {
        // 5 windows → remove one → the remaining four keep relative order.
        let (p0, _s0) = arrange(&wins(5), area(), gap(), border(), None);
        let four: Vec<WindowId> = wins(5).into_iter().filter(|&w| w != 3).collect();
        let (p1, _s1) = arrange(&four, area(), gap(), border(), None);

        let rel_before = |a: WindowId, b: WindowId| -> (std::cmp::Ordering, std::cmp::Ordering) {
            let ra = p0.iter().find(|(w, _)| *w == a).unwrap().1;
            let rb = p0.iter().find(|(w, _)| *w == b).unwrap().1;
            (
                center_x(ra).cmp(&center_x(rb)),
                center_y(ra).cmp(&center_y(rb)),
            )
        };
        let rel_after = |a: WindowId, b: WindowId| -> (std::cmp::Ordering, std::cmp::Ordering) {
            let ra = p1.iter().find(|(w, _)| *w == a).unwrap().1;
            let rb = p1.iter().find(|(w, _)| *w == b).unwrap().1;
            (
                center_x(ra).cmp(&center_x(rb)),
                center_y(ra).cmp(&center_y(rb)),
            )
        };
        for a in [1u32, 2, 4, 5] {
            for b in [1u32, 2, 4, 5] {
                if a == b {
                    continue;
                }
                assert_eq!(rel_before(a, b), rel_after(a, b));
            }
        }
    }

    #[test]
    fn smart_gaps_single_window_collapses_to_zero() {
        // With smart_gaps and a single window the engine receives (0,0) gaps,
        // so the cell fills the un-inset workarea exactly.
        let full = Rect::new(0, 0, 1920, 1080);
        let (p, _) = arrange(&wins(1), full, 0, border(), None);
        let r = p[0].1;
        assert_eq!((r.x, r.y), (full.x, full.y));
        assert_eq!(
            (r.w as i32 + 2 * border(), r.h as i32 + 2 * border()),
            (full.w as i32, full.h as i32)
        );
    }
}
