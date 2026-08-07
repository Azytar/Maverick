// maverick/src/core/present.rs
// Presentation layer: turns the pure layout geometry (layout_rect) produced by
// `core::layout::arrange` into the final geometry applied to X11 (rendered_rect).
//
// There is a presentation overlay per workspace that is *not* tied to focus:
//
//   * FULLSCREEN — a fullscreen window covers the whole `screen` (border 0),
//     ignoring reserved regions, and is raised above everything.
//   * MAXIMIZED — a maximized window fills the `workarea` (screen minus
//     reserved regions) with border 0. It is raised like fullscreen but never
//     paints over reserved regions.
//
// A window in either mode is presented as long as its flags say so, focused or
// not. The tiles underneath are still computed by the layout (unchanged), so
// exiting the overlay restores the workspace exactly where it was — and because
// the geometry does not depend on focus, moving focus never resizes anything.
// The renderer layers the *focused* tile above the overlay (peek) or moves the
// focus entirely without resizing the presented window.
//
// Fullscreen takes precedence over maximized if a window somehow has both flags.

use crate::core::layout::Placements;
use crate::types::{Monitor, Rect, State, WindowId};

#[cfg(test)]
use crate::core::layout::LayoutRegistry;

/// Rewrite `placements` in place, applying the presentation overlay for `mon`.
/// Returns every presented window in `placements` order (focused last, so the
/// caller can raise them in that order and the focused one lands on top).
pub fn present(state: &State, mon: &Monitor, placements: &mut Placements) -> Vec<WindowId> {
    let mut raise = Vec::new();
    for entry in placements.iter_mut() {
        let win = entry.0;
        let Some(client) = state.clients.get(&win) else {
            continue;
        };
        // (target rect, target border). Fullscreen wins over maximized.
        let present_rect: Option<(Rect, u32)> = if client.is_fullscreen() {
            Some((mon.screen, 0))
        } else if client.is_maximized() {
            Some((mon.workarea, 0))
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
    raise
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
        let registry = LayoutRegistry::new();
        crate::core::layout::arrange(&state, 0, &cfg, &registry, &mut p);
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
        crate::core::layout::arrange(&state, 0, &cfg, &registry, &mut p);
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
            .set(WinFlags::MAXIMIZED);
        state.monitors[0].focused = Some(1);

        let mut p = Placements::new();
        let registry = LayoutRegistry::new();
        crate::core::layout::arrange(&state, 0, &cfg, &registry, &mut p);
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
    fn unfocused_maximized_fills_workarea() {
        let (mut state, cfg) = setup();
        add(&mut state, 1);
        add(&mut state, 2);
        state.monitors[0].workarea = Rect::new(0, 22, 800, 578);
        state
            .clients
            .get_mut(&1)
            .unwrap()
            .flags
            .set(WinFlags::MAXIMIZED);
        state.monitors[0].focused = Some(2);

        let mut p = Placements::new();
        let registry = LayoutRegistry::new();
        crate::core::layout::arrange(&state, 0, &cfg, &registry, &mut p);
        let raised = present(&state, &state.monitors[0], &mut p);

        assert_eq!(raised, vec![1], "unfocused maximized stays presented");
        let (_, rect, _) = p.iter().find(|e| e.0 == 1).copied().unwrap();
        assert_eq!(rect, state.monitors[0].workarea);
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
        let registry = LayoutRegistry::new();
        crate::core::layout::arrange(&state, 0, &cfg, &registry, &mut p);
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
        let registry = LayoutRegistry::new();
        crate::core::layout::arrange(&state, 0, &cfg, &registry, &mut p);
        let snapshot = p.clone();
        let raised = present(&state, &state.monitors[0], &mut p);

        assert!(raised.is_empty());
        assert_eq!(p, snapshot);
    }
}
