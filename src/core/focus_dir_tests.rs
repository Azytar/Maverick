//! `Workspace::presented_maximize` is *derived* state: it names the monitor's
//! focused window when that window is maximized on either axis and lives on the
//! monitor's active workspace. `State::sync_presented_maximize` is the single
//! writer of the derivation, so every mutation of `Monitor::focused` has to call
//! it — otherwise the field keeps naming a window that no longer holds the focus
//! and the whole presentation layer (`core::present` geometry,
//! `render::stack_overlay` ordering, `presented_overlay_owner` → `best_focus`)
//! answers from a window the user is no longer looking at.
//!
//! These tests drive the real command (`FocusDirection`) rather than a helper
//! that mimics the backend sink, so they fail if a `mon.focused` write in
//! `commands.rs` forgets the sync. `State::check_invariants` cannot stand in for
//! that: its `presented_maximize` clauses only test the field against the client's
//! own maximize flags and workspace, and a stale entry naming a still-maximized
//! window on the active workspace passes all of them — so the engine's debug
//! `assert_invariants` runs clean over the broken state and the incoherence is
//! only observable through the readers below.

use crate::config::Cfg;
use crate::core::commands::FocusDirection;
use crate::core::Engine;
use crate::types::{Client, Dir, Monitor, Rect, State, WinFlags, WindowId};

fn default_cfg() -> Cfg {
    Cfg {
        border_w: 2,
        gaps_inner: 6,
        gaps_outer: 6,
        smart_gaps: false,
        corner_radius: 0,
        n_tags: 9,
        column_width: 0.6,
        focus_mouse: false,
        warp_cursor: false,
        accordion_boost: 0.30,
        overview_zoom_min: 0.25,
        col_normal: 0,
        col_focused: 0,
        col_urgent: 0,
        tag_names: (1..=9).map(|n| n.to_string()).collect(),
        keybinds: vec![],
        rules: vec![],
        autostart: vec![],
        ..Default::default()
    }
}

fn setup_engine() -> Engine {
    let mut engine = Engine::new(default_cfg());
    engine
        .state
        .monitors
        .push(Monitor::new(Rect::new(0, 0, 1920, 1080), 9));
    engine
}

/// Register `win` as a managed client placed on the selected monitor's active
/// workspace, the way `manage()` does before tiling it.
fn add_client(engine: &mut Engine, win: WindowId) {
    let mi = engine.state.sel_mon;
    let ws_i = engine.state.monitors[mi].active_index();
    let mut c = Client::new(win, mi, engine.state.monitors[mi].workspaces[ws_i].id);
    c.border_w = engine.cfg.border_w;
    engine.state.add_client(c);
}

/// Two side-by-side columns, `wins[0]` in the first one, for `Left`/`Right`.
fn two_columns(engine: &mut Engine, wins: [WindowId; 2]) {
    for (i, &win) in wins.iter().enumerate() {
        add_client(engine, win);
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_index();
        if i == 0 {
            let mut col = crate::types::Column::new(1.0);
            col.windows.push(win);
            engine.state.monitors[mi].workspaces[ws_i].columns.push(col);
            engine.state.monitors[mi].workspaces[ws_i].focus.column_idx = 0;
        } else {
            engine.state.monitors[mi].workspaces[ws_i].add_tiled(win, 0.6);
        }
    }
}

/// One column holding both windows as rows, for `Up`/`Down`.
fn two_rows(engine: &mut Engine, wins: [WindowId; 2]) {
    for &win in &wins {
        add_client(engine, win);
    }
    let mi = engine.state.sel_mon;
    let ws_i = engine.state.monitors[mi].active_index();
    let mut col = crate::types::Column::new(1.0);
    col.windows.extend_from_slice(&wins);
    col.focused = 0;
    engine.state.monitors[mi].workspaces[ws_i].columns.push(col);
    engine.state.monitors[mi].workspaces[ws_i].focus.column_idx = 0;
}

fn set_maximized_vert(engine: &mut Engine, win: WindowId) {
    engine
        .state
        .clients
        .get_mut(&win)
        .expect("managed client")
        .flags
        .set(WinFlags::MAXIMIZED_V);
}

/// The half of `Backend::focus()` that a pure command cannot perform: the real-X
/// sink that repairs `mon.focused`/`presented_maximize` for the paths which only
/// emit `Effect::FocusWindow`. Reproduced here so a fixture starts from a state a
/// live session is already in.
fn focus_via_sink(engine: &mut Engine, win: WindowId) {
    let mi = engine
        .state
        .clients
        .get(&win)
        .map_or(engine.state.sel_mon, |c| c.monitor);
    engine.state.sel_mon = mi;
    {
        let mon = &mut engine.state.monitors[mi];
        mon.focused = Some(win);
        mon.focus_stack.retain(|&w| w != win);
        mon.focus_stack.push(win);
    }
    engine.state.sync_presented_maximize(mi);
}

fn active_presented_maximize(engine: &Engine) -> Option<WindowId> {
    let mon = &engine.state.monitors[engine.state.sel_mon];
    mon.workspaces[mon.active_index()].presented_maximize
}

/// The engine's own readers of the derived field, checked on the state
/// `Engine::execute` hands back — i.e. before any backend effect runs.
fn assert_overlay_readers_track_focus(state: &State, mi: usize, ctx: &str) {
    let mon = &state.monitors[mi];
    let owner = mon.workspaces[mon.active_index()].presented_maximize;
    assert_eq!(
        state.presented_overlay_owner(mi),
        owner,
        "{ctx}: presented_overlay_owner and presented_maximize disagree"
    );
    if let Some(w) = owner {
        assert_eq!(
            mon.focused,
            Some(w),
            "{ctx}: presented_maximize owner {w} is not the focused window {:?}",
            mon.focused
        );
    }
}

/// Navigating off the focused maximized window must take its presentation
/// overlay down: `presented_maximize` has to follow `mon.focused`, not lag one
/// focus change behind.
#[test]
fn focus_direction_away_from_a_maximized_window_drops_its_overlay() {
    for (dir, grid) in [
        (Dir::Right, true),
        (Dir::Left, true),
        (Dir::Down, false),
        (Dir::Up, false),
    ] {
        let mut engine = setup_engine();
        if grid {
            two_columns(&mut engine, [1, 2]);
        } else {
            two_rows(&mut engine, [1, 2]);
        }
        set_maximized_vert(&mut engine, 1);
        focus_via_sink(&mut engine, 1);
        assert_eq!(
            active_presented_maximize(&engine),
            Some(1),
            "{dir:?} fixture: the focused maximized window owns the overlay"
        );

        engine.execute(FocusDirection(dir));

        assert_eq!(
            engine.state.monitors[0].focused,
            Some(2),
            "{dir:?} moved the focus to the neighbour"
        );
        assert_eq!(
            active_presented_maximize(&engine),
            None,
            "{dir:?} left the unfocused maximized window 1 as presented_maximize owner"
        );
        assert_overlay_readers_track_focus(&engine.state, 0, &format!("{dir:?}"));
    }
}

/// Mirror case: navigating onto a *different* maximized window must make THAT
/// window the owner, not keep the old one.
#[test]
fn focus_direction_onto_a_maximized_window_makes_it_the_owner() {
    let mut engine = setup_engine();
    two_columns(&mut engine, [1, 2]);
    set_maximized_vert(&mut engine, 1);
    focus_via_sink(&mut engine, 1);
    assert_eq!(active_presented_maximize(&engine), Some(1));

    engine.execute(FocusDirection(Dir::Right));
    assert_eq!(engine.state.monitors[0].focused, Some(2));
    assert_eq!(
        active_presented_maximize(&engine),
        None,
        "the unfocused non-maximized window 2 owns nothing"
    );

    engine
        .state
        .clients
        .get_mut(&2)
        .expect("client 2")
        .flags
        .set(WinFlags::MAXIMIZED_H);
    engine.execute(FocusDirection(Dir::Left));
    engine.execute(FocusDirection(Dir::Right));
    assert_eq!(engine.state.monitors[0].focused, Some(2));
    assert_eq!(
        active_presented_maximize(&engine),
        Some(2),
        "the newly focused maximized window must own the overlay"
    );
    assert_overlay_readers_track_focus(&engine.state, 0, "onto a new owner");
}

/// `Dir::Next`/`Dir::Prev` walk the focus stack rather than the column grid but
/// write `mon.focused` on the same path, so they carry the same obligation.
#[test]
fn focus_direction_next_prev_keeps_the_overlay_owner_with_the_focus() {
    for dir in [Dir::Next, Dir::Prev] {
        let mut engine = setup_engine();
        two_columns(&mut engine, [1, 2]);
        set_maximized_vert(&mut engine, 1);
        // Both windows in the focus stack, with 1 on top and focused, so a stack
        // walk in either direction lands on 2.
        focus_via_sink(&mut engine, 2);
        focus_via_sink(&mut engine, 1);
        assert_eq!(active_presented_maximize(&engine), Some(1));

        engine.execute(FocusDirection(dir));

        assert_eq!(
            engine.state.monitors[0].focused,
            Some(2),
            "{dir:?} walked the stack to the other window"
        );
        assert_eq!(
            active_presented_maximize(&engine),
            None,
            "{dir:?} left the unfocused maximized window 1 as presented_maximize owner"
        );
        assert_overlay_readers_track_focus(&engine.state, 0, &format!("{dir:?}"));
    }
}

/// `best_focus` short-circuits on `presented_overlay_owner`, so a stale owner
/// makes it answer with a window that is neither focused nor presented. That is
/// what `ViewWorkspace`/`MoveToWorkspace`/`focus_best` consume.
#[test]
fn focus_direction_off_a_maximized_window_does_not_leave_best_focus_stale() {
    let mut engine = setup_engine();
    two_columns(&mut engine, [1, 2]);
    set_maximized_vert(&mut engine, 1);
    focus_via_sink(&mut engine, 1);

    engine.execute(FocusDirection(Dir::Right));

    assert_eq!(engine.state.monitors[0].focused, Some(2));
    assert_overlay_readers_track_focus(&engine.state, 0, "best_focus");
    assert_eq!(
        engine.state.best_focus(0),
        Some(2),
        "best_focus answered from the stale overlay owner"
    );
    // `check_invariants` passes either way: it constrains `presented_maximize`
    // against the client's own flags/workspace, never against `mon.focused`, so
    // it cannot be the guard for this. Asserted to document that.
    engine
        .state
        .check_invariants()
        .expect("invariants after FocusDirection off a maximized window");
}
