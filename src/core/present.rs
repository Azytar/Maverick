// maverick/src/core/present.rs
// Presentation layer: turns the pure layout geometry (layout_rect) produced by
// `core::layout::arrange` into the final geometry applied to X11 (rendered_rect).
//
// There is a presentation overlay per workspace that is *not* tied to focus:
//
//   * FULLSCREEN (only in `LayoutKind::Grid`) — a fullscreen window covers the
//     whole `screen` (border 0), ignoring reserved regions, and is raised above
//     everything. In `LayoutKind::Column` a fullscreen window is NOT an overlay:
//     it is a normal participant of the scrolling ribbon (see `core::layout`),
//     so it scrolls with the camera and can be scrolled away from — the niri
//     behaviour.
//   * MAXIMIZED — a maximized window fills the `workarea` (screen minus
//     reserved regions) with border 0, on the axes it actually asked for.
//     `_NET_WM_STATE_MAXIMIZED_VERT` and `_..._HORZ` are two independent EWMH
//     states, so a vertical-only maximize stretches y/h and leaves x/w at the
//     tile the layout produced (and vice versa). It is raised like fullscreen
//     but never paints over reserved regions, and only while it is the focused
//     window.
//
// A maximized window in either layout is presented as long as its flags say so,
// focused or not. The tiles underneath are still computed by the layout
// (unchanged), so exiting the overlay restores the workspace exactly where it
// was — and because the geometry does not depend on focus, moving focus never
// resizes anything. The renderer layers the *focused* tile above the overlay
// (peek) or moves the focus entirely without resizing the presented window.
//
// Fullscreen takes precedence over maximized if a window somehow has both flags.

use crate::core::layout::Placements;
use crate::types::{LayoutKind, Monitor, Rect, State, WindowId};

#[cfg(test)]
use crate::core::layout::LayoutRegistry;
#[cfg(test)]
use crate::core::layout::RibbonScratch;

/// Rewrite `placements` in place, applying the presentation overlay for `mon`,
/// and collect every presented window into `raise` (cleared first) in
/// `placements` order — focused last, so the caller can raise them in that
/// order and the focused one lands on top.
///
/// `raise` is caller-owned rather than returned because the two production
/// callers (`arrange_full_phase` and the compositor's `live_placements`) both
/// discard it, and `live_placements` runs once per animating monitor *per
/// frame*. Returning a fresh `Vec` there was a heap allocation on every frame
/// of every scroll, for a value nobody read.
pub fn present_into(state: &State, mon: &Monitor, placements: &mut Placements, raise: &mut Vec<WindowId>) {
    raise.clear();
    for entry in placements.iter_mut() {
        let win = entry.0;
        let tile = entry.1;
        let Some(client) = state.clients.get(&win) else {
            continue;
        };
        // (target rect, target border). Fullscreen wins over maximized.
        let present_rect: Option<(Rect, u32)> = if client.is_fullscreen()
            && (mon.ws().layout == LayoutKind::Grid || client.is_true_fullscreen())
        {
            // In the `Column` layout a fullscreen window is a *participant of the
            // scrolling ribbon* (laid out by `core::layout`), not a pinned
            // overlay — so it is only presented as an overlay in `Grid`, where
            // there is no ribbon for it to join.
            //
            // The exception is `FullscreenPolicy::True` (games): that fullscreen
            // is exclusive by definition, covers the screen in *any* layout, and
            // is excluded from `fs_ctx` so it never joins the ribbon at all.
            Some((mon.screen, 0))
        } else if (client.is_maximized_v() || client.is_maximized_h()) && mon.focused == Some(win) {
            // Per-axis maximize: `maximized_rect` only stretches the axes that
            // are actually on (a vertical-only maximize fills the workarea's
            // height but keeps its tile width), so `_NET_WM_STATE_MAXIMIZED_VERT`
            // no longer silently promotes to a full maximize.
            Some((maximized_rect(tile, mon.workarea, client), 0))
        } else {
            None
        };
        if let Some((rect, bw)) = present_rect {
            entry.1 = rect;
            entry.2 = bw;
            raise.push(win);
        }
    }
    // Focused presented window last: among several presented windows the focused
    // one must be the topmost after the caller raises the whole list in order.
    if let Some(f) = mon.focused {
        if let Some(pos) = raise.iter().position(|w| *w == f) {
            let win = raise.remove(pos);
            raise.push(win);
        }
    }
}

/// [`present_into`] with an owned result. Convenience for tests and for callers
/// that genuinely want the list; the per-frame paths use `present_into`.
#[cfg(test)]
pub fn present(state: &State, mon: &Monitor, placements: &mut Placements) -> Vec<WindowId> {
    let mut raise = Vec::new();
    present_into(state, mon, placements, &mut raise);
    raise
}

/// The workarea, clipped to the axes the client actually maximized on.
///
/// EWMH models vertical and horizontal maximization as two independent states,
/// so `_NET_WM_STATE_MAXIMIZED_VERT` alone must only stretch y/h — the
/// x/width stay at whatever the layout gave the tile. Collapsing both into one
/// "fill the workarea" rect (what a single `MAXIMIZED` flag forced) turned
/// every vertical maximize into a full one.
fn maximized_rect(tile: Rect, workarea: Rect, client: &crate::types::Client) -> Rect {
    let (x, w) = if client.is_maximized_h() {
        (workarea.x, workarea.w)
    } else {
        (tile.x, tile.w)
    };
    let (y, h) = if client.is_maximized_v() {
        (workarea.y, workarea.h)
    } else {
        (tile.y, tile.h)
    };
    Rect::new(x, y, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Cfg;
    use crate::types::{Client, LayoutKind, Monitor, Rect, State, WinFlags};

    fn setup() -> (State, Cfg) {
        let mut state = State::new();
        let mut mon = Monitor::new(Rect::new(0, 0, 800, 600), 1);
        mon.workarea = Rect::new(0, 0, 800, 600);
        // Fullscreen is only a pinned overlay in `Grid` (in `Column` it joins the
        // scrolling ribbon), so these overlay tests run in `Grid`.
        mon.workspaces[0].layout = LayoutKind::Grid;
        state.monitors.push(mon);
        (state, Cfg::default())
    }

    fn add(state: &mut State, win: WindowId) {
        let mut c = Client::new(win, 0, 0);
        c.geom = Rect::new(0, 0, 100, 100);
        state.add_client(c);
        state.monitors[0].workspaces[0].add_tiled(win, 0.5);
    }

    #[test]
    fn focused_fullscreen_covers_screen() {
        let (mut state, cfg) = setup();
        add(&mut state, 1);
        add(&mut state, 2);
        state
            .clients
            .get_mut(&1)
            .unwrap()
            .flags
            .set(WinFlags::FULLSCREEN);
        state.monitors[0].focused = Some(1);

        let mut p = Placements::new();
        let registry = LayoutRegistry::new();

crate::core::layout::arrange(&state, 
            0,
            &cfg,
            &registry,
            crate::core::layout::Phase::Live,
            &mut p,
            &mut RibbonScratch::default(),
        );
        let raised = present(&state, &state.monitors[0], &mut p);

        assert_eq!(raised, vec![1]);
        let (_, rect, bw) = p.iter().find(|e| e.0 == 1).copied().unwrap();
        assert_eq!(rect, state.monitors[0].screen);
        assert_eq!(bw, 0);
    }

    #[test]
    fn fullscreen_persists_while_unfocused() {
        let (mut state, cfg) = setup();
        add(&mut state, 1);
        add(&mut state, 2);
        // Window 1 is fullscreen; focus is on 2. The overlay must not shrink.
        state
            .clients
            .get_mut(&1)
            .unwrap()
            .flags
            .set(WinFlags::FULLSCREEN);
        state.monitors[0].focused = Some(2);

        let mut p = Placements::new();
        let registry = LayoutRegistry::new();

crate::core::layout::arrange(&state, 
            0,
            &cfg,
            &registry,
            crate::core::layout::Phase::Live,
            &mut p,
            &mut RibbonScratch::default(),
        );
        let raised = present(&state, &state.monitors[0], &mut p);

        assert_eq!(raised, vec![1], "unfocused fullscreen stays presented");
        let (_, rect, bw) = p.iter().find(|e| e.0 == 1).copied().unwrap();
        assert_eq!(
            rect, state.monitors[0].screen,
            "unfocused fullscreen must still cover the whole screen"
        );
        assert_eq!(bw, 0);
    }

    #[test]
    fn focused_maximized_fills_workarea() {
        let (mut state, cfg) = setup();
        add(&mut state, 1);
        add(&mut state, 2);
        // Reserve a 22px top region so workarea != screen.
        state.monitors[0].workarea = Rect::new(0, 22, 800, 578);
        state
            .clients
            .get_mut(&1)
            .unwrap()
            .flags
            .set(WinFlags::MAXIMIZED_V | WinFlags::MAXIMIZED_H);
        state.monitors[0].focused = Some(1);

        let mut p = Placements::new();
        let registry = LayoutRegistry::new();

crate::core::layout::arrange(&state, 
            0,
            &cfg,
            &registry,
            crate::core::layout::Phase::Live,
            &mut p,
            &mut RibbonScratch::default(),
        );
        let raised = present(&state, &state.monitors[0], &mut p);

        assert_eq!(raised, vec![1]);
        let (_, rect, bw) = p.iter().find(|e| e.0 == 1).copied().unwrap();
        assert_eq!(rect, state.monitors[0].workarea);
        assert_ne!(
            rect, state.monitors[0].screen,
            "maximized must respect reserved regions"
        );
        assert_eq!(bw, 0, "maximized uses border 0 so it never overflows");
    }

    #[test]
    fn unfocused_maximized_returns_to_tile_slot() {
        let (mut state, cfg) = setup();
        add(&mut state, 1);
        add(&mut state, 2);
        state.monitors[0].workarea = Rect::new(0, 22, 800, 578);
        state
            .clients
            .get_mut(&1)
            .unwrap()
            .flags
            .set(WinFlags::MAXIMIZED_V | WinFlags::MAXIMIZED_H);
        state.monitors[0].focused = Some(2);

        let mut p = Placements::new();
        let registry = LayoutRegistry::new();

crate::core::layout::arrange(&state, 
            0,
            &cfg,
            &registry,
            crate::core::layout::Phase::Live,
            &mut p,
            &mut RibbonScratch::default(),
        );
        let raised = present(&state, &state.monitors[0], &mut p);

        assert!(
            !raised.contains(&1),
            "unfocused maximized must not be presented as an overlay"
        );
        let (_, rect, _) = p.iter().find(|e| e.0 == 1).copied().unwrap();
        assert_ne!(
            rect, state.monitors[0].workarea,
            "unfocused maximized must keep its tile slot, not the whole workarea"
        );
        assert!(
            rect.w < state.monitors[0].workarea.w,
            "unfocused maximized slot is narrower than the workarea"
        );
    }

    #[test]
    fn fullscreen_beats_maximized() {
        let (mut state, cfg) = setup();
        add(&mut state, 1);
        state.monitors[0].workarea = Rect::new(0, 22, 800, 578);
        let c = state.clients.get_mut(&1).unwrap();
        c.flags.set(WinFlags::MAXIMIZED_V | WinFlags::MAXIMIZED_H);
        c.flags.set(WinFlags::FULLSCREEN);
        state.monitors[0].focused = Some(1);

        let mut p = Placements::new();
        let registry = LayoutRegistry::new();

crate::core::layout::arrange(&state, 
            0,
            &cfg,
            &registry,
            crate::core::layout::Phase::Live,
            &mut p,
            &mut RibbonScratch::default(),
        );
        present(&state, &state.monitors[0], &mut p);

        let (_, rect, bw) = p.iter().find(|e| e.0 == 1).copied().unwrap();
        assert_eq!(
            rect, state.monitors[0].screen,
            "fullscreen wins over maximized"
        );
        assert_eq!(bw, 0);
    }

    // ── Fase 3: the two EWMH maximize axes are independent ────────────────

    /// Present window 1 with the given axis flags and return (tile, presented).
    fn present_axes(v: bool, h: bool) -> (Rect, Rect, Rect) {
        let (mut state, cfg) = setup();
        add(&mut state, 1);
        add(&mut state, 2);
        // A 22px top reservation, so workarea != screen on the vertical axis.
        state.monitors[0].workarea = Rect::new(0, 22, 800, 578);
        let c = state.clients.get_mut(&1).unwrap();
        if v {
            c.flags.set(WinFlags::MAXIMIZED_V);
        }
        if h {
            c.flags.set(WinFlags::MAXIMIZED_H);
        }
        state.monitors[0].focused = Some(1);

        let mut p = Placements::new();
        let registry = LayoutRegistry::new();

crate::core::layout::arrange(&state, 
            0,
            &cfg,
            &registry,
            crate::core::layout::Phase::Live,
            &mut p,
            &mut RibbonScratch::default(),
        );
        let tile = p.iter().find(|e| e.0 == 1).copied().unwrap().1;
        present(&state, &state.monitors[0], &mut p);
        let presented = p.iter().find(|e| e.0 == 1).copied().unwrap().1;
        (tile, presented, state.monitors[0].workarea)
    }

    #[test]
    fn maximize_vertical_only_stretches_y() {
        let (tile, presented, wa) = present_axes(true, false);
        assert_eq!(
            (presented.y, presented.h),
            (wa.y, wa.h),
            "vertical maximize must fill the workarea height"
        );
        assert_eq!(
            (presented.x, presented.w),
            (tile.x, tile.w),
            "vertical maximize must NOT touch x/width — that is the whole \
             point of splitting the axes"
        );
        assert!(presented.w < wa.w, "the tile is narrower than the workarea");
    }

    #[test]
    fn maximize_horizontal_only_stretches_x() {
        let (tile, presented, wa) = present_axes(false, true);
        assert_eq!(
            (presented.x, presented.w),
            (wa.x, wa.w),
            "horizontal maximize must fill the workarea width"
        );
        assert_eq!(
            (presented.y, presented.h),
            (tile.y, tile.h),
            "horizontal maximize must NOT touch y/height"
        );
    }

    #[test]
    fn maximize_both_axes_fills_the_workarea() {
        let (_, presented, wa) = present_axes(true, true);
        assert_eq!(
            presented, wa,
            "both axes together are the classic full maximize"
        );
    }

    #[test]
    fn no_fullscreen_is_noop() {
        let (mut state, cfg) = setup();
        add(&mut state, 1);
        state.monitors[0].focused = Some(1);

        let mut p = Placements::new();
        let registry = LayoutRegistry::new();

crate::core::layout::arrange(&state, 
            0,
            &cfg,
            &registry,
            crate::core::layout::Phase::Live,
            &mut p,
            &mut RibbonScratch::default(),
        );
        let snapshot = p.clone();
        let raised = present(&state, &state.monitors[0], &mut p);

        assert!(raised.is_empty());
        assert_eq!(p, snapshot);
    }
}
