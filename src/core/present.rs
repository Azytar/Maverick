// maverick/src/core/present.rs
// Presentation layer: turns the pure layout geometry (layout_rect) produced by
// `core::layout::arrange` into the final geometry applied to X11 (rendered_rect).
//
// There are two presentation modes today, both niri-style and tied to focus:
//
//   * FULLSCREEN — a fullscreen window that IS the monitor's focused window
//     covers the whole `screen` (border 0), ignoring reserved regions, and is
//     raised above everything.
//   * MAXIMIZED — a maximized focused window fills the `workarea` (screen minus
//     reserved regions) keeping its border. It is raised like fullscreen but
//     never paints over reserved regions.
//
// A window in either mode that is NOT focused keeps its normal layout_rect — it
// is just another tiled/floating window until it regains focus. Because the
// mapping is derived from focus here (and not stored in the layout), moving
// focus can never leave a stale presented window covering the screen. That
// removes the need to block movement while a window is presented.
//
// Fullscreen takes precedence over maximized if a window somehow has both flags.

use crate::core::layout::Placements;
use crate::types::{Monitor, Rect, State, WindowId};

/// Rewrite `placements` in place, applying presentation modes for `mon`.
/// Returns the window that must be raised to the top (the focused fullscreen or
/// maximized window), if any.
pub fn present(state: &State, mon: &Monitor, placements: &mut Placements) -> Option<WindowId> {
    let focused = mon.focused?;
    let client = state.clients.get(&focused)?;

    // (target rect, target border). Fullscreen wins over maximized.
    let present_rect: Option<(Rect, Option<u32>)> = if client.is_fullscreen() {
        Some((mon.screen, Some(0)))
    } else if client.is_maximized() {
        Some((mon.workarea, None)) // None = keep the window's layout border
    } else {
        None
    };

    let (rect, border) = present_rect?;

    for entry in placements.iter_mut() {
        if entry.0 == focused {
            entry.1 = rect;
            if let Some(bw) = border {
                entry.2 = bw;
            }
            return Some(focused);
        }
    }
    // Focused presented window isn't in this monitor's placements (e.g. float
    // path). Still report it so the caller can raise it.
    Some(focused)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Cfg;
    use crate::core::layout::arrange;
    use crate::types::{Client, LayoutKind, Monitor, Rect, State, WinFlags};

    fn setup() -> (State, Cfg) {
        let mut state = State::new();
        let mut mon = Monitor::new(Rect::new(0, 0, 800, 600), 1);
        mon.workarea = Rect::new(0, 0, 800, 600);
        mon.workspaces[0].layout = LayoutKind::Column;
        state.monitors.push(mon);
        (state, Cfg::default())
    }

    fn add(state: &mut State, win: WindowId) {
        let mut c = Client::new(win, 0, 0);
        c.geom = Rect::new(0, 0, 100, 100);
        state.add_client(c);
        state.monitors[0].workspaces[0].add_tiled(win, 0, 800);
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
        arrange(&state, 0, &cfg, &mut p);
        let raised = present(&state, &state.monitors[0], &mut p);

        assert_eq!(raised, Some(1));
        let (_, rect, bw) = p.iter().find(|e| e.0 == 1).copied().unwrap();
        assert_eq!(rect, state.monitors[0].screen);
        assert_eq!(bw, 0);
    }

    #[test]
    fn unfocused_fullscreen_keeps_layout_rect() {
        let (mut state, cfg) = setup();
        add(&mut state, 1);
        add(&mut state, 2);
        // Window 1 is fullscreen but focus is on 2.
        state
            .clients
            .get_mut(&1)
            .unwrap()
            .flags
            .set(WinFlags::FULLSCREEN);
        state.monitors[0].focused = Some(2);

        let mut p = Placements::new();
        arrange(&state, 0, &cfg, &mut p);
        let before = p.iter().find(|e| e.0 == 1).copied().unwrap();
        let raised = present(&state, &state.monitors[0], &mut p);

        assert_eq!(raised, None);
        let after = p.iter().find(|e| e.0 == 1).copied().unwrap();
        assert_eq!(
            before, after,
            "unfocused fullscreen must keep its layout rect"
        );
        assert_ne!(after.1, state.monitors[0].screen);
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
            .set(WinFlags::MAXIMIZED);
        state.monitors[0].focused = Some(1);

        let mut p = Placements::new();
        arrange(&state, 0, &cfg, &mut p);
        let raised = present(&state, &state.monitors[0], &mut p);

        assert_eq!(raised, Some(1));
        let (_, rect, _) = p.iter().find(|e| e.0 == 1).copied().unwrap();
        assert_eq!(rect, state.monitors[0].workarea);
        assert_ne!(
            rect, state.monitors[0].screen,
            "maximized must respect reserved regions"
        );
    }

    #[test]
    fn fullscreen_beats_maximized() {
        let (mut state, cfg) = setup();
        add(&mut state, 1);
        state.monitors[0].workarea = Rect::new(0, 22, 800, 578);
        let c = state.clients.get_mut(&1).unwrap();
        c.flags.set(WinFlags::MAXIMIZED);
        c.flags.set(WinFlags::FULLSCREEN);
        state.monitors[0].focused = Some(1);

        let mut p = Placements::new();
        arrange(&state, 0, &cfg, &mut p);
        present(&state, &state.monitors[0], &mut p);

        let (_, rect, bw) = p.iter().find(|e| e.0 == 1).copied().unwrap();
        assert_eq!(
            rect, state.monitors[0].screen,
            "fullscreen wins over maximized"
        );
        assert_eq!(bw, 0);
    }

    #[test]
    fn no_fullscreen_is_noop() {
        let (mut state, cfg) = setup();
        add(&mut state, 1);
        state.monitors[0].focused = Some(1);

        let mut p = Placements::new();
        arrange(&state, 0, &cfg, &mut p);
        let snapshot = p.clone();
        let raised = present(&state, &state.monitors[0], &mut p);

        assert_eq!(raised, None);
        assert_eq!(p, snapshot);
    }
}
