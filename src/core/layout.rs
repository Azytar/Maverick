// maverick/src/layout.rs
// Niri-inspired columnar layout engine.
//
// Key idea: coordinates are COMPUTED, never stored.
// Column positions = f(scroll offset, column widths, gap).
// No mutable geom drift — every arrange() is a pure function over State.

use crate::config::Cfg;
use crate::types::{LayoutKind, Monitor, Rect, State};
use x11rb::protocol::xproto::Window;

pub type Placements = Vec<(Window, Rect, u32)>; // (win, geom, border_w)

/// P10: Clear and refill `out` instead of allocating a new Vec each call.
pub fn arrange(state: &State, mon_idx: usize, cfg: &Cfg, out: &mut Placements) {
    let mon = &state.monitors[mon_idx];
    let layout = mon.ws().layout;
    out.clear();
    match layout {
        LayoutKind::Column => arrange_columns(state, mon, cfg, out),
        LayoutKind::Monocle => arrange_monocle(state, mon, cfg, out),
        LayoutKind::Grid => arrange_grid(state, mon, cfg, out),
    }
}

// ─── Column layout ────────────────────────────────────────────────────────────
//
// Each column sits at a fixed x position (derived from scroll + sum of prior
// col widths + gaps). Windows within a column split vertically.
// The focused window in a focused column gets a larger share (split_bias).

fn arrange_columns(state: &State, mon: &Monitor, cfg: &Cfg, out: &mut Placements) {
    let ws = mon.ws();
    let wa = mon.workarea;
    let gap = cfg.gaps as i32;
    let bw = cfg.border_w as i32;
    let bias = cfg.split_bias;

    // ── tiled windows ──
    let mut col_x = wa.x - ws.scroll;
    for (ci, col) in ws.columns.iter().enumerate() {
        let col_w = col.width as i32;
        let n = col.windows.len();
        if n == 0 {
            col_x += col_w + gap;
            continue;
        }

        let focused_col = ci == ws.focus.column_idx;
        let inner_w = (col_w - 2 * bw - 2 * gap).max(1);
        let total_h = wa.h as i32 - 2 * gap;
        let base_h = if n > 1 { total_h / n as i32 } else { total_h };

        // ── Precompute (row_h, row_y) for every row in O(N) ──────────────
        // The old code recomputed the sum of all preceding row heights on
        // every iteration for the focused-split case → O(N²) per column.
        // Now we do a single forward pass (O(N)) and store the results.
        let row_info: Vec<(i32, i32)> = if n == 1 {
            vec![(total_h.max(1), wa.y + gap)]
        } else if focused_col {
            let bonus = (total_h as f32 * bias * 0.1) as i32;
            let deficit = if n > 1 { bonus / (n as i32 - 1) } else { 0 };
            let mut acc = Vec::with_capacity(n);
            let mut y = wa.y + gap;
            for i in 0..n {
                let h = if i == col.focused {
                    (base_h + bonus).max(1)
                } else {
                    (base_h - deficit).max(1)
                };
                acc.push((h, y));
                y += h + gap;
            }
            acc
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
            let client = match state.clients.get(&win) {
                Some(c) => c,
                None => continue,
            };
            if client.is_fullscreen() {
                out.push((win, mon.screen, 0));
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

    // ── floating windows — keep existing geom, just clamp to workarea ──
    for &win in &ws.floats {
        let client = match state.clients.get(&win) {
            Some(c) => c,
            None => continue,
        };
        if client.is_fullscreen() {
            out.push((win, mon.screen, 0));
        } else {
            let g = client.geom;
            out.push((win, g, cfg.border_w));
        }
    }
}

// ─── Monocle layout ───────────────────────────────────────────────────────────

fn arrange_monocle(state: &State, mon: &Monitor, cfg: &Cfg, out: &mut Placements) {
    let ws = mon.ws();
    let wa = mon.workarea;

    let all_wins: Vec<Window> = ws
        .columns
        .iter()
        .flat_map(|c| c.windows.iter().copied())
        .chain(ws.floats.iter().copied())
        .collect();

    for win in all_wins {
        if state.clients.contains_key(&win) {
            out.push((win, Rect::new(wa.x, wa.y, wa.w, wa.h), cfg.border_w));
        }
    }
}

// ─── Grid layout ─────────────────────────────────────────────────────────────

fn arrange_grid(state: &State, mon: &Monitor, cfg: &Cfg, out: &mut Placements) {
    let ws = mon.ws();
    let wa = mon.workarea;
    let gap = cfg.gaps as i32;
    let bw = cfg.border_w as i32;

    let wins: Vec<Window> = ws
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
            out.push((win, c.geom, cfg.border_w));
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

    let gap = cfg.gaps as i32;
    let wa_w = mon.workarea.w as i32;

    // x of focused column (relative to virtual origin).
    // Usar saturating_add para evitar overflow con muchas columnas.
    let col_x_virtual: i32 = ws.columns[..col_idx]
        .iter()
        .map(|c| (c.width as i32).saturating_add(gap))
        .fold(0i32, |a, b| a.saturating_add(b));

    let focused_w = ws.columns[col_idx].width as i32;
    let focused_center = col_x_virtual.saturating_add(focused_w / 2);
    let screen_center = wa_w / 2;

    (focused_center.saturating_sub(screen_center)).max(0)
}
