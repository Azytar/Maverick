//! Presentation overlay — turns layout geometry into final X geometry.
//!
//! `present_into` is the single place that rewrites `layout::arrange`'s
//! `layout_rect` into the `rendered_rect` the reconciler applies to X11 — in
//! place. Coordinate computation (`layout::arrange` + `ribbon_geom`), the
//! reconciler diff against `AppliedState`, and the backend
//! `ConfigureWindow`/restack calls all live elsewhere.
//!
//! Invariants: `fullscreen > maximized` — fullscreen (checked via
//! `is_fullscreen_overlay`) covers `mon.screen` with border 0 and wins if both
//! flags are set; otherwise the maximized window (`presented_maximize`) fills
//! `mon.workarea` with border 0, per-axis (vert/horz independently) via
//! `maximized_rect`. In `LayoutKind::Column` a tiled fullscreen is a ribbon
//! participant, not a pinned overlay (see `layout::FsCtx`); exclusive
//! `FullscreenPolicy::True` is always an overlay. Tiles underneath are still
//! computed unchanged, so exiting the overlay restores the workspace exactly.
//!
//! "Presented" means the rect the reconciler writes. It does not promise *when*
//! the server has processed it, nor anything about stacking relative to the
//! dock or other overlays.

use crate::core::layout::Placements;
use crate::types::{Monitor, Rect, State};

#[cfg(test)]
use crate::core::layout::RibbonScratch;

/// Rewrite `placements` in place, applying the presentation overlay for `mon`.
/// Precedence is `fullscreen > maximized`: `is_fullscreen_overlay()` rewrites
/// to `mon.screen` (border 0) and wins if both flags are set; otherwise
/// `presented_maximize` rewrites via `maximized_rect` (per-axis, workarea,
/// border 0). The stacking order among presented windows is decided by the
/// backend's `stack_overlay`, not here.
pub fn present_into(state: &State, mon: &Monitor, placements: &mut Placements) {
    for entry in placements.iter_mut() {
        let win = entry.0;
        let tile = entry.1;
        let Some(client) = state.clients.get(&win) else {
            continue;
        };
        // (target rect, target border). Fullscreen wins over maximized.
        let present_rect: Option<(Rect, u32)> = if client.is_fullscreen_overlay() {
            // A fullscreen window is a *participant of the scrolling ribbon*
            // (laid out by `core::layout`), not a pinned overlay, so it only
            // presents as an overlay in a non-`Column` layout, which has no
            // ribbon for it to join.
            //
            // The exception is `FullscreenPolicy::True` (games): that fullscreen
            // is exclusive by definition, covers the screen in *any* layout, and
            // is excluded from `fs_ctx` so it never joins the ribbon at all.
            Some((mon.screen, 0))
        } else if mon.ws().presented_maximize == Some(win) {
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
        }
    }
}

/// [`present_into`] as a statement, for tests.
#[cfg(test)]
pub fn present(state: &State, mon: &Monitor, placements: &mut Placements) {
    present_into(state, mon, placements);
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
    use crate::types::{Client, LayoutKind, Monitor, Rect, State, WinFlags, WindowId};

    fn setup() -> (State, Cfg) {
        let mut state = State::new();
        let mut mon = Monitor::new(Rect::new(0, 0, 800, 600), 1);
        mon.workarea = Rect::new(0, 0, 800, 600);
        // These are overlay tests, and the overlay tests below only reach the
        // overlay path by asking for exclusive fullscreen
        // (`FullscreenPolicy::True`, set per test) — so the layout stays
        // `Column` and a non-exclusive fullscreen would join the ribbon.
        mon.workspaces[0].layout = LayoutKind::Column;
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
        state.clients.get_mut(&1).unwrap().fullscreen_policy = crate::types::FullscreenPolicy::True;
        state.monitors[0].focused = Some(1);

        let mut p = Placements::new();

        crate::core::layout::arrange(&state, 0, &cfg, &mut p, &mut RibbonScratch::default());
        present(&state, &state.monitors[0], &mut p);

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
        state.clients.get_mut(&1).unwrap().fullscreen_policy = crate::types::FullscreenPolicy::True;
        state.monitors[0].focused = Some(2);

        let mut p = Placements::new();

        crate::core::layout::arrange(&state, 0, &cfg, &mut p, &mut RibbonScratch::default());
        present(&state, &state.monitors[0], &mut p);

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
        state.monitors[0].workspaces[0].presented_maximize = Some(1);

        let mut p = Placements::new();

        crate::core::layout::arrange(&state, 0, &cfg, &mut p, &mut RibbonScratch::default());
        present(&state, &state.monitors[0], &mut p);

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

        crate::core::layout::arrange(&state, 0, &cfg, &mut p, &mut RibbonScratch::default());
        present(&state, &state.monitors[0], &mut p);

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
        c.fullscreen_policy = crate::types::FullscreenPolicy::True;
        state.monitors[0].focused = Some(1);
        state.monitors[0].workspaces[0].presented_maximize = Some(1);

        let mut p = Placements::new();

        crate::core::layout::arrange(&state, 0, &cfg, &mut p, &mut RibbonScratch::default());
        present(&state, &state.monitors[0], &mut p);

        let (_, rect, bw) = p.iter().find(|e| e.0 == 1).copied().unwrap();
        assert_eq!(
            rect, state.monitors[0].screen,
            "fullscreen wins over maximized"
        );
        assert_eq!(bw, 0);
    }

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
        state.monitors[0].workspaces[0].presented_maximize = Some(1);

        let mut p = Placements::new();

        crate::core::layout::arrange(&state, 0, &cfg, &mut p, &mut RibbonScratch::default());
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

        crate::core::layout::arrange(&state, 0, &cfg, &mut p, &mut RibbonScratch::default());
        let snapshot = p.clone();
        present(&state, &state.monitors[0], &mut p);

        assert_eq!(p, snapshot);
    }
}

/// Property-based coverage of the presentation overlay.
///
/// `present_into` is the only place that rewrites `layout::arrange`'s tile
/// rects into the rects the reconciler writes to X11, so its contract is small
/// and sharp:
///
/// * it rewrites *in place* — the placement set itself never changes;
/// * `fullscreen > maximized`, and a presented window is always configured
///   with border 0 so the overlay it covers cannot overflow;
/// * EWMH's two maximize axes are independent: a vertical maximize stretches
///   y/h and leaves x/w at whatever the tile had, and vice versa;
/// * only the workspace's *presented* maximize is an overlay — an unfocused
///   maximized window returns to its tile slot;
/// * in `LayoutKind::Column` a normal-policy fullscreen is a ribbon
///   participant, not a pinned overlay.
#[cfg(test)]
mod proptests {
    use super::*;
    use crate::config::Cfg;
    use crate::core::layout::{arrange, Placements, RibbonScratch};
    use crate::types::{
        Client, Column, Edge, Focus, FullscreenPolicy, Monitor, Rect, SizeHints, State, WinFlags,
        WindowId,
    };
    use proptest::prelude::*;

    /// One window's presentation policy: what the overlay layer branches on,
    /// plus the float inputs `arrange` normalizes with.
    #[derive(Debug, Clone, Copy)]
    struct WindowSpec {
        fullscreen: bool,
        exclusive: bool,
        max_v: bool,
        max_h: bool,
        border_w: u32,
        floating: bool,
        /// The WM adopted the client's own rect: `float_client_authority`.
        sealed: bool,
        geom: Rect,
        hints: SizeHints,
    }

    /// A generated scene: one monitor, a ribbon of columns and a few floats.
    #[derive(Debug, Clone)]
    struct Scene {
        screen: Rect,
        reserved: Vec<(Edge, u32)>,
        gaps_inner: u32,
        gaps_outer: u32,
        border_w: u32,
        /// `(weight, rows)` for the tiled columns.
        columns: Vec<(f32, usize)>,
        /// One spec per client, in window-id order from 1.
        windows: Vec<WindowSpec>,
        /// Index into `windows` the monitor focuses.
        focus: usize,
        /// Make that window the workspace's presented maximize.
        present_maximize: bool,
    }

    fn screen_rect() -> impl Strategy<Value = Rect> {
        (
            -1920i32..=1920,
            -1080i32..=1080,
            prop_oneof![0u32..=1, 1u32..=64, 320u32..=3840],
            prop_oneof![0u32..=1, 1u32..=64, 240u32..=2160],
        )
            .prop_map(|(x, y, w, h)| Rect::new(x, y, w, h))
    }

    /// `WM_NORMAL_HINTS` a client can declare, including the unsatisfiable
    /// combinations the float projection has to survive.
    fn size_hints() -> impl Strategy<Value = SizeHints> {
        (
            any::<bool>(),
            0i32..=64,
            0i32..=64,
            0i32..=256,
            0i32..=256,
            0i32..=16,
            0i32..=16,
            0i32..=64,
        )
            .prop_map(|(valid, min_w, min_h, max_w, max_h, inc_w, inc_h, base)| {
                SizeHints {
                    valid,
                    min_w,
                    min_h,
                    max_w,
                    max_h,
                    inc_w,
                    inc_h,
                    base_w: base,
                    base_h: base,
                    min_aspect: 0.0,
                    max_aspect: 0.0,
                    flags: 0,
                }
            })
    }

    fn scene() -> impl Strategy<Value = Scene> {
        let window = (
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            0u32..=8,
            any::<bool>(),
            any::<bool>(),
            screen_rect(),
            size_hints(),
        )
            .prop_map(
                |(fullscreen, exclusive, max_v, max_h, border_w, floating, sealed, geom, hints)| {
                    WindowSpec {
                        fullscreen,
                        exclusive,
                        max_v,
                        max_h,
                        border_w,
                        floating,
                        sealed,
                        geom,
                        hints,
                    }
                },
            );
        (
            screen_rect(),
            prop::collection::vec(
                (
                    prop_oneof![
                        Just(Edge::Top),
                        Just(Edge::Bottom),
                        Just(Edge::Left),
                        Just(Edge::Right)
                    ],
                    0u32..=120,
                ),
                0..=2,
            ),
            0u32..=16,
            0u32..=16,
            0u32..=8,
            prop::collection::vec((0.05f32..=1.0, 0usize..=4), 1..=3),
            prop::collection::vec(window, 1..=6),
            0usize..=5,
            any::<bool>(),
        )
            .prop_map(
                |(
                    screen,
                    reserved,
                    gaps_inner,
                    gaps_outer,
                    border_w,
                    columns,
                    windows,
                    focus,
                    present_maximize,
                )| {
                    let focus = if windows.is_empty() {
                        0
                    } else {
                        focus % windows.len()
                    };
                    Scene {
                        screen,
                        reserved,
                        gaps_inner,
                        gaps_outer,
                        border_w,
                        columns,
                        windows,
                        focus,
                        present_maximize,
                    }
                },
            )
    }

    impl Scene {
        /// Build the state the overlay layer reads. Windows are assigned
        /// round-robin to the columns, so a scene has both tiled clients and
        /// (for the floating specs) floats, and every client is registered.
        fn state(&self) -> State {
            let mut state = State::new();
            state.monitors.push(Monitor::new(self.screen, 2));
            for &(edge, thickness) in &self.reserved {
                state.monitors[0].set_reserved_region(0xD0C, edge, thickness);
            }
            let mut tiled: Vec<WindowId> = Vec::new();
            for (i, spec) in self.windows.iter().enumerate() {
                let win = (i + 1) as WindowId;
                let mut c = Client::new(win, 0, 0);
                c.geom = spec.geom;
                c.hints = spec.hints;
                c.border_w = spec.border_w;
                c.float_client_authority = spec.sealed;
                c.fullscreen_policy = if spec.exclusive {
                    FullscreenPolicy::True
                } else {
                    FullscreenPolicy::Normal
                };
                if spec.fullscreen {
                    c.flags.set(WinFlags::FULLSCREEN);
                }
                if spec.max_v {
                    c.flags.set(WinFlags::MAXIMIZED_V);
                }
                if spec.max_h {
                    c.flags.set(WinFlags::MAXIMIZED_H);
                }
                if spec.floating {
                    c.flags.set(WinFlags::FLOAT);
                }
                state.add_client(c);
                if spec.floating {
                    state.monitors[0].workspaces[0].floats.push(win);
                } else {
                    tiled.push(win);
                }
            }
            // Fill the generated `(weight, rows)` columns from the tiled
            // clients in order, so a scene exercises both one-row and stacked
            // columns; leftover tiled clients fall into a fresh column so no
            // client is ever dropped from the tree.
            if !tiled.is_empty() {
                let ws = &mut state.monitors[0].workspaces[0];
                let mut next = 0usize;
                for &(weight, rows) in &self.columns {
                    let end = (next + rows).min(tiled.len());
                    ws.columns.push(Column {
                        windows: tiled[next..end].to_vec(),
                        focused: 0,
                        weight,
                    });
                    next = end;
                }
                for &win in &tiled[next..] {
                    ws.columns.push(Column {
                        windows: vec![win],
                        focused: 0,
                        weight: 1.0,
                    });
                }
                ws.focus = Focus { column_idx: 0 };
            }
            if !self.windows.is_empty() {
                state.monitors[0].focused = Some((self.focus + 1) as WindowId);
            }
            let focus = state.monitors[0].focused.unwrap_or(0);
            if self.present_maximize {
                let presented = state
                    .clients
                    .get(&focus)
                    .is_some_and(maverick_core::Client::is_maximized);
                if presented {
                    state.monitors[0].workspaces[0].presented_maximize = Some(focus);
                }
            }
            state
        }

        fn cfg(&self) -> Cfg {
            Cfg {
                gaps_inner: self.gaps_inner,
                gaps_outer: self.gaps_outer,
                border_w: self.border_w,
                ..Cfg::default()
            }
        }
    }

    /// Arrange and then present, returning the tile rects alongside the
    /// presented ones so a test can tell which entries the overlay rewrote.
    fn project(state: &State, cfg: &Cfg) -> (Placements, Placements) {
        let mut out = Placements::new();
        arrange(state, 0, cfg, &mut out, &mut RibbonScratch::default());
        let tiles = out.clone();
        present_into(state, &state.monitors[0], &mut out);
        (tiles, out)
    }

    /// `present_into` rewrites rects *in place*: the placement set has to
    /// survive it untouched — every window `arrange` produced is still there,
    /// in the same order. A dropped or reordered entry would make the
    /// reconciler diff a different set than the layout produced.
    #[test]
    fn presenting_preserves_the_placement_set() {
        proptest!(|(s in scene())| {
            let state = s.state();
            let cfg = s.cfg();
            let (tiles, presented) = project(&state, &cfg);

            let tile_windows: Vec<WindowId> = tiles.iter().map(|e| e.0).collect();
            let presented_windows: Vec<WindowId> = presented.iter().map(|e| e.0).collect();
            prop_assert_eq!(
                &presented_windows[..],
                &tile_windows[..],
                "the overlay must not add, drop or reorder placements"
            );

            // Every window the overlay rewrote is configured with border 0: a
            // fullscreen overlay covers the screen and a maximize fills the
            // workarea, and either one that kept its border would overflow the
            // area it covers.
            for (&(win, rect, bw), &(_, tile_rect, tile_bw)) in presented.iter().zip(tiles.iter()) {
                if rect != tile_rect || bw != tile_bw {
                    prop_assert_eq!(bw, 0, "presented window {} kept a border", win);
                }
            }
        });
    }

    /// The overlay is a projection, not a state transition: presenting an
    /// already-presented placement must be a no-op, so the reconciler's diff
    /// against `AppliedState` is empty and no `ConfigureWindow` is emitted for
    /// a window that did not change.
    #[test]
    fn presenting_an_already_presented_placement_changes_nothing() {
        proptest!(|(s in scene())| {
            let state = s.state();
            let cfg = s.cfg();
            let (_, once) = project(&state, &cfg);
            let mut twice_placements = once.clone();
            present_into(&state, &state.monitors[0], &mut twice_placements);
            prop_assert_eq!(
                twice_placements, once,
                "presenting twice moved a window that was already presented"
            );
        });
    }

    /// EWMH models vertical and horizontal maximization as two independent
    /// states, and clients do request only one of them, so a per-axis maximize
    /// stretches exactly the axis it owns: the vertical one takes y/h from the
    /// workarea and leaves x/w at whatever the tile had, the horizontal one
    /// takes x/w and leaves y/h. Collapsing both into "fill the workarea" is
    /// the bug this split exists to prevent.
    #[test]
    fn a_maximize_presentation_stretches_only_the_axis_it_owns() {
        proptest!(|(s in scene())| {
            let mut s = s;
            // Exactly one presented maximize, so the assertion is about the
            // axes and not about which window won the overlay, and no other
            // window is fullscreen (a fullscreen column hides its siblings, so
            // the focused window would have no tile to stretch).
            s.present_maximize = true;
            for w in &mut s.windows {
                w.fullscreen = false;
                w.exclusive = false;
            }
            let f = s.focus;
            s.windows[f].floating = false;
            let state = s.state();
            let cfg = s.cfg();
            let mon_focus = state.monitors[0].focused;
            prop_assume!(mon_focus.is_some());
            let focus = mon_focus.unwrap();
            let presented_max = state.monitors[0].ws().presented_maximize;
            prop_assume!(presented_max == Some(focus));
            let client = state.clients.get(&focus).unwrap();
            prop_assume!(!client.is_fullscreen_overlay());
            let (v, h) = (client.is_maximized_v(), client.is_maximized_h());

            let (tiles, out) = project(&state, &cfg);
            let tile = tiles.iter().find(|e| e.0 == focus).unwrap().1;
            let got = out.iter().find(|e| e.0 == focus).unwrap().1;
            let wa = state.monitors[0].workarea;
            if v {
                prop_assert_eq!(
                    (got.y, got.h),
                    (wa.y, wa.h),
                    "a vertical maximize must fill the workarea height: {:?} wa={:?}",
                    got,
                    wa
                );
            } else {
                prop_assert_eq!(
                    (got.y, got.h),
                    (tile.y, tile.h),
                    "a non-vertical maximize must not touch y/h: {:?} vs {:?}",
                    got,
                    tile
                );
            }
            if h {
                prop_assert_eq!(
                    (got.x, got.w),
                    (wa.x, wa.w),
                    "a horizontal maximize must fill the workarea width: {:?} wa={:?}",
                    got,
                    wa
                );
            } else {
                prop_assert_eq!(
                    (got.x, got.w),
                    (tile.x, tile.w),
                    "a non-horizontal maximize must not touch x/w: {:?} vs {:?}",
                    got,
                    tile
                );
            }
        });
    }

    /// `fullscreen > maximized` is the overlay's precedence rule: a window
    /// that carries both flags must still be presented as covering
    /// `mon.screen` — the screen, not the strut-inset workarea — with border 0,
    /// so nothing shows through the reserved region.
    #[test]
    fn fullscreen_wins_over_maximized_and_covers_the_screen() {
        proptest!(|(s in scene())| {
            let mut s = s;
            s.present_maximize = true;
            // Make the focused window both fullscreen (exclusively) and
            // maximized on both axes, and leave every other window out of the
            // overlay entirely so the assertion is about the precedence rule.
            for w in &mut s.windows {
                w.fullscreen = false;
                w.exclusive = false;
                w.max_v = false;
                w.max_h = false;
            }
            let f = s.focus;
            s.windows[f].floating = false;
            s.windows[f].fullscreen = true;
            s.windows[f].exclusive = true;
            s.windows[f].max_v = true;
            s.windows[f].max_h = true;
            let state = s.state();
            let cfg = s.cfg();
            let focus = state.monitors[0].focused.unwrap();
            prop_assume!(state.monitors[0].ws().presented_maximize == Some(focus));

            let (_, out) = project(&state, &cfg);
            let entry = out.iter().find(|e| e.0 == focus).unwrap();
            prop_assert_eq!(
                entry.1,
                state.monitors[0].screen,
                "fullscreen must beat maximized and cover the screen"
            );
            prop_assert_eq!(entry.2, 0, "a fullscreen overlay carries no border");
        });
    }

    /// Only the workspace's *presented* maximize is an overlay. A maximized
    /// window that does not own the presentation — an unfocused one, or one the
    /// workspace has not adopted — must keep its tile slot, or every
    /// background window that ever asked to be maximized would cover the
    /// focused one.
    #[test]
    fn only_the_presented_maximize_becomes_an_overlay() {
        proptest!(|(s in scene())| {
            let mut s = s;
            s.present_maximize = false;
            let state = s.state();
            let cfg = s.cfg();
            let (tiles, out) = project(&state, &cfg);
            prop_assume!(state.monitors[0].ws().presented_maximize.is_none());
            for &(win, _, _) in &out {
                let client = state.clients.get(&win).unwrap();
                if client.is_maximized() && !client.is_fullscreen_overlay() {
                    let tile = tiles.iter().find(|e| e.0 == win).unwrap();
                    let kept = out.iter().find(|e| e.0 == win).unwrap();
                    prop_assert_eq!(
                        (kept.1, kept.2),
                        (tile.1, tile.2),
                        "window {} returned to its tile slot, not to the workarea",
                        win
                    );
                }
            }
        });
    }

    /// In `LayoutKind::Column` a normal-policy fullscreen is a *ribbon
    /// participant* — one screen-filling tile that scrolls with the camera —
    /// not a pinned overlay. Only `FullscreenPolicy::True` is exclusive, and
    /// only that kind reaches the overlay path. Letting a normal-policy
    /// fullscreen through would pin it over the workspace and break the
    /// documented h/l scroll between fullscreen columns.
    #[test]
    fn a_column_fullscreen_is_a_ribbon_participant_not_an_overlay() {
        proptest!(|(s in scene())| {
            let mut s = s;
            // The focused window is the workspace's only fullscreen, so it is
            // also the fullscreen window of its column and therefore the tile
            // that survives the column's sibling hiding.
            for w in &mut s.windows {
                w.fullscreen = false;
                w.exclusive = false;
            }
            let f = s.focus;
            s.windows[f].fullscreen = true;
            s.windows[f].exclusive = false;
            s.windows[f].max_v = false;
            s.windows[f].max_h = false;
            let state = s.state();
            let cfg = s.cfg();
            let focus = state.monitors[0].focused.unwrap();
            let client = state.clients.get(&focus).unwrap();
            prop_assert!(!client.is_fullscreen_overlay());
            prop_assert!(state.monitors[0].ws().presented_maximize.is_none());

            let (tiles, out) = project(&state, &cfg);
            let tile = tiles.iter().find(|e| e.0 == focus).unwrap();
            let kept = out.iter().find(|e| e.0 == focus).unwrap();
            prop_assert_eq!(
                (kept.1, kept.2),
                (tile.1, tile.2),
                "a ribbon fullscreen must keep the tile the layout computed for it"
            );
        });
    }
}
