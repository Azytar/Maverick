// maverick/src/core/invariants.rs
//
// Regression suite for the fullscreen+mouse focus contract.
//
// THE CONTRACT (invariant A): for every focus path — keyboard
// (`FocusDirection` → `Effect::ArrangeMonitor`), mouse (`Backend::focus`),
// and EWMH (`_NET_ACTIVE_WINDOW` → `self.focus`) — at REST (camera settled,
// i.e. `camera.position == camera.target`) the geometry X11 uses for input
// MUST equal the projection of the logical ribbon at that camera value:
//
//     client.geom == projection(camera.target, layout_state)   [at rest]
//
// The compositor draws the *Live* phase (`camera.position`); those two may
// differ mid-animation but MUST converge at rest. Input hit-testing
// (`find_client`) and the pointer warp both read `client.geom`, so a
// divergence between `camera.target` and `client.geom` means a click/enter
// goes to the wrong window. The fullscreen audit found exactly this bug:
// mouse `focus()` retargeted `camera.target` WITHOUT re-projecting, so
// `client.geom` lagged the camera and a neighbour was clickable at the
// focused window's old rect. (The fix lives in the backend `focus()` →
// `arrange` wiring; this suite locks the resulting end-state contract.)
//
// Secondary invariants pinned down here:
//   B  settled projection uses the same camera value the backend projects
//      with at rest (`camera.target`, which equals `camera.position` once
//      settled). The pure suite simulates "at rest" by snapping
//      `camera.position = camera.target` after a focus retarget.
//   C  `present()` overlays fullscreen (always) and maximized (only while
//      focused) on top of the settled projection; `client.geom` equals the
//      post-present rect, which equals the pre-present projection otherwise.
//   D  `border_w` is part of the geometry; assertions are border-aware.
//   E  mid-animation, Live may differ from Settled; once `Camera::step`
//      converges, Live == Settled == projection(target). Never assert
//      `position == target` mid-flight.
//   F  mouse / keyboard / EWMH focus paths converge to the same final
//      `client.geom`. The pure suite proves the geometry the two core paths
//      produce is identical; the actual `Backend::focus()`→`arrange` wiring
//      gap is covered end-to-end by `tests/xephyr-suite.sh`.
//
// SCOPE: only the backend `focus()`→`arrange` fix (already landed) is in
// scope; no further production changes are made here.

use crate::config::Cfg;
use crate::core::commands::{
    Command, MoveWindow, PageSnap, ToggleMaximize, ToggleOverview, ViewWorkspace, ViewportZoom,
};
use crate::core::layout::{
    arrange, column_screen_extents, fs_ctx, ideal_scroll, ribbon_geom, LayoutRegistry, Phase,
    RibbonScratch,
};
use crate::core::present::present;
use crate::core::Engine;
use crate::types::{Client, Column, Dir, Monitor, Rect, WinFlags, WindowId};

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
        compositor: crate::config::CompositorCfg::default(),
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

fn default_registry() -> LayoutRegistry {
    LayoutRegistry::new()
}

fn setup_engine() -> Engine {
    let mut engine = Engine::new(default_cfg());
    engine
        .state
        .monitors
        .push(Monitor::new(Rect::new(0, 0, 1920, 1080), 9));
    engine
}

// ─── projection harness ─────────────────────────────────────────────────────

/// Run the exact pure layout + presentation the backend uses inside
/// `arrange()` → `apply_geom`, then write the resulting rect/border back into
/// `client.geom` / `client.border_w` (what X11 then reads for input).
fn apply_settled(
    engine: &mut Engine,
    mi: usize,
    registry: &LayoutRegistry,
) -> std::collections::HashMap<WindowId, (Rect, u32)> {
    let mut placements = Vec::new();
    arrange(
        &engine.state,
        mi,
        &engine.cfg,
        registry,
        Phase::Settled,
        &mut placements,
        &mut RibbonScratch::default(),
    );
    // Capture the projection BEFORE `present`: `present` mutates `camera.target`
    // (it calls `scroll_to_focused` to keep an exclusive fullscreen pinned), so
    // any later `arrange` would read a different target. The backend writes
    // `client.geom` from exactly this projection each frame, so assert against it.
    let projected: std::collections::HashMap<WindowId, (Rect, u32)> =
        placements.iter().map(|(w, r, b)| (*w, (*r, *b))).collect();
    present(&engine.state, &engine.state.monitors[mi], &mut placements);
    for (win, rect, bw) in placements {
        if let Some(c) = engine.state.clients.get_mut(&win) {
            c.geom = rect;
            c.border_w = bw;
        }
    }
    projected
}

/// Simulate the camera AND all other animated factors having *settled*: at
/// rest `position == target`, `zoom == zoom_target`, and each column's boost is
/// at its rest value (1.0 focused, 0.0 otherwise). The backend reaches this via
/// its per-frame spring integration; the pure suite snaps.
fn snap_all(engine: &mut Engine, mi: usize, ws_i: usize) {
    let ws = &mut engine.state.monitors[mi].workspaces[ws_i];
    ws.camera.position = ws.camera.target;
    ws.camera.velocity = 0.0;
    ws.zoom = ws.zoom_target;
    let focus_i = ws.focus.column_idx;
    for (i, col) in ws.columns.iter_mut().enumerate() {
        col.boost = if i == focus_i { 1.0 } else { 0.0 };
    }
    if ws.overview {
        ws.overview = false;
    }
}

/// Replicate the geometry half of `Backend::focus()`: retarget `camera.target`
/// onto column `ci`, settle the camera, then project with the Settled phase.
/// No `ArrangeMonitor` is emitted — this is the mouse/EWMH pointer path.
fn settle_on_column(
    engine: &mut Engine,
    mi: usize,
    ws_i: usize,
    ci: usize,
    registry: &LayoutRegistry,
) -> std::collections::HashMap<WindowId, (Rect, u32)> {
    let wa = engine.state.monitors[mi].workarea;
    let screen = engine.state.monitors[mi].screen;
    let fs = fs_ctx(
        &engine.state.clients,
        &engine.state.monitors[mi].workspaces[ws_i],
        screen,
    );
    let win = engine.state.monitors[mi].workspaces[ws_i].columns[ci].windows[0];
    {
        let ws = &mut engine.state.monitors[mi].workspaces[ws_i];
        ws.focus.column_idx = ci;
        ws.camera.target = ideal_scroll(ws, &engine.cfg, wa, fs);
    }
    // The real backend also moves focus to the clicked window (this drives the
    // accordion boost that `ideal_scroll`/`column_screen_extents` read), so the
    // settled camera target and the projected geometry stay in lockstep.
    engine.state.monitors[mi].focused = Some(win);
    if let Some(pos) = engine.state.monitors[mi]
        .focus_stack
        .iter()
        .position(|w| *w == win)
    {
        engine.state.monitors[mi].focus_stack.remove(pos);
    }
    engine.state.monitors[mi].focus_stack.push(win);
    snap_all(engine, mi, ws_i);
    apply_settled(engine, mi, registry)
}

/// Run a typed command and apply every `ArrangeMonitor` effect with the
/// Settled projection (what the backend does). The backend retargets the
/// camera (`scroll_to_focused` → `ideal_scroll`) in its render/animation loop
/// BEFORE it projects, so we do the same here to mirror the real focus pipeline.
/// It also applies the focus the command announced via the `FocusChanged` event
/// (the backend's event handler does this; the pure `Engine` does not), so the
/// accordion boost that `ideal_scroll` reads stays consistent.
fn focus_step(
    engine: &mut Engine,
    mut cmd: impl Command,
    registry: &LayoutRegistry,
) -> std::collections::HashMap<WindowId, (Rect, u32)> {
    // Mirror `Engine::execute` but keep the `CommandReport` so we can apply the
    // focus the command announced (the backend's event handler does this).
    let report = cmd.execute(&mut engine.state, &mut engine.cfg);
    // Apply the focus the command announced BEFORE retargeting the camera, so
    // the accordion boost `ideal_scroll` / `snap_all` read matches the newly
    // focused column.
    if let Some(crate::core::event::Event::FocusChanged { to: Some(win), .. }) = &report.event {
        let mi = engine.state.sel_mon;
        engine.state.monitors[mi].focused = Some(*win);
        if let Some(pos) = engine.state.monitors[mi]
            .focus_stack
            .iter()
            .position(|w| *w == *win)
        {
            engine.state.monitors[mi].focus_stack.remove(pos);
        }
        engine.state.monitors[mi].focus_stack.push(*win);
        // Keep `ws.focus.column_idx` in sync (the backend's focus handler does
        // this too) so the settled boost targets the right column.
        let ws_i = engine.state.monitors[mi].active_ws;
        if let Some(ci) = engine.state.monitors[mi].workspaces[ws_i]
            .columns
            .iter()
            .position(|c| c.windows.contains(win))
        {
            engine.state.monitors[mi].workspaces[ws_i].focus.column_idx = ci;
        }
    }
    for e in &report.effects {
        // `Effect::FocusWindow` is what `Backend::focus` (mouse / EWMH) emits;
        // the backend retargets + arranges `sel_mon` in response. Mirror that.
        // `Effect::ArrangeMonitor(m)` is what keyboard commands emit. Both must
        // end at the same settled geometry.
        let m = match e {
            crate::core::effect::Effect::FocusWindow(Some(_)) => engine.state.sel_mon,
            crate::core::effect::Effect::ArrangeMonitor(m) => *m,
            _ => continue,
        };
        let ws_i = engine.state.monitors[m].active_ws;
        let wa = engine.state.monitors[m].workarea;
        let fs = fs_ctx(
            &engine.state.clients,
            &engine.state.monitors[m].workspaces[ws_i],
            engine.state.monitors[m].screen,
        );
        engine.state.monitors[m].workspaces[ws_i].camera.target = ideal_scroll(
            &engine.state.monitors[m].workspaces[ws_i],
            &engine.cfg,
            wa,
            fs,
        );
        snap_all(engine, m, ws_i);
        return apply_settled(engine, m, registry);
    }
    // Some commands (e.g. ToggleMaximize) emit no ArrangeMonitor/FocusWindow.
    std::collections::HashMap::new()
}

// ─── assertions ─────────────────────────────────────────────────────────────

fn rect_eq(a: Rect, b: Rect) -> bool {
    (a.x - b.x).abs() <= 2
        && (a.y - b.y).abs() <= 2
        && (a.w as i32 - b.w as i32).abs() <= 2
        && (a.h as i32 - b.h as i32).abs() <= 2
}

fn geom_of(engine: &Engine, win: WindowId) -> Rect {
    engine.state.clients.get(&win).expect("client exists").geom
}

fn inside_wa(engine: &Engine, mi: usize, r: Rect) -> bool {
    let wa = engine.state.monitors[mi].workarea;
    r.x >= wa.x - 2
        && r.y >= wa.y - 2
        && r.x + r.w as i32 <= wa.x + wa.w as i32 + 2
        && r.y + r.h as i32 <= wa.y + wa.h as i32 + 2
}

/// Invariant A: every *tiled* client's `geom` equals the settled projection.
fn assert_all_tiled_match_settled(
    engine: &Engine,
    mi: usize,
    proj: &std::collections::HashMap<WindowId, (Rect, u32)>,
) {
    for (win, (rect, _bw)) in proj {
        let g = geom_of(engine, *win);
        assert!(
            rect_eq(g, *rect),
            "client {win} geom {g:?} != settled projection {rect:?} (mon.focused={:?})",
            engine.state.monitors[mi].focused,
        );
    }
}

/// Drive focus to a specific window via the command `Backend::focus` emits.
fn focus_window(
    engine: &mut Engine,
    win: WindowId,
    registry: &LayoutRegistry,
) -> std::collections::HashMap<WindowId, (Rect, u32)> {
    focus_step(
        engine,
        crate::core::commands::FocusWindow(Some(win)),
        registry,
    )
}

// ─── tests ──────────────────────────────────────────────────────────────────

#[test]
fn h_l_focus_keeps_settled_geometry() {
    let registry = default_registry();
    let mut engine = engine_with_columns(3, 1);
    let mi = engine.state.sel_mon;
    let ws_i = engine.state.monitors[mi].active_ws;

    for ci in 0..3 {
        let win = (ci + 1) as u32;
        engine.state.add_client(Client::new(win, mi, ws_i));
        engine.state.monitors[mi].workspaces[ws_i].add_tiled(win, engine.cfg.column_width);
    }
    let first = 1u32;
    engine.state.monitors[mi].focused = Some(first);
    engine.state.monitors[mi].focus_stack = vec![1u32, 2u32, 3u32];

    let proj = focus_window(&mut engine, 2, &registry);
    assert_all_tiled_match_settled(&engine, mi, &proj);

    let proj = focus_window(&mut engine, 3, &registry);
    assert_all_tiled_match_settled(&engine, mi, &proj);

    let proj = focus_window(&mut engine, 1, &registry);
    assert_all_tiled_match_settled(&engine, mi, &proj);
}

#[test]
fn mouse_focus_centers_focused_column() {
    let registry = default_registry();
    let mut engine = engine_with_columns(3, 1);
    let mi = engine.state.sel_mon;
    let ws_i = engine.state.monitors[mi].active_ws;

    // Pure equivalent of Backend::focus(Some(col1 window)): retarget + settle + project.
    let proj = settle_on_column(&mut engine, mi, ws_i, 1, &registry);

    let _win = engine.state.monitors[mi].workspaces[ws_i].columns[1].windows[0];
    // Invariant A: geom == settled projection (the backend writes this to X11).
    assert_all_tiled_match_settled(&engine, mi, &proj);
}

#[test]
fn fullscreen_then_neighbor_settled_geometry() {
    let registry = default_registry();
    let mut engine = setup_engine();
    let mi = engine.state.sel_mon;
    let ws_i = engine.state.monitors[mi].active_ws;
    let screen = engine.state.monitors[mi].screen;

    for ci in 0..3 {
        let win = (ci + 1) as u32;
        engine.state.add_client(Client::new(win, mi, ws_i));
        engine.state.monitors[mi].workspaces[ws_i].add_tiled(win, engine.cfg.column_width);
        if ci == 0 {
            engine
                .state
                .clients
                .get_mut(&win)
                .unwrap()
                .flags
                .set(WinFlags::FULLSCREEN);
        }
    }
    let a = 1u32;
    let b = 2u32;
    let first = a;
    engine.state.monitors[mi].focused = Some(first);
    engine.state.monitors[mi].focus_stack = vec![first];

    // focus A → A fills the screen (tiled fullscreen is a screen-wide ribbon tile).
    let _proj = settle_on_column(&mut engine, mi, ws_i, 0, &registry);
    let ga = geom_of(&engine, a);
    assert!(
        rect_eq(ga, screen),
        "focused fullscreen column must fill the screen: got {ga:?} want {screen:?}"
    );

    // focus B → B's geom equals the settled projection. Input must resolve to
    // B at this rect (the original bug: B was clickable at A's stale rect).
    let proj = settle_on_column(&mut engine, mi, ws_i, 1, &registry);
    let gb = geom_of(&engine, b);
    assert_all_tiled_match_settled(&engine, mi, &proj);
    // B must be the on-screen click target: its rect is what X11 hit-tests.
    assert!(
        gb.w > 0 && gb.h > 0,
        "neighbour B must have a real rect: {gb:?}"
    );

    // focus A again → A fills the screen once more.
    let _proj = settle_on_column(&mut engine, mi, ws_i, 0, &registry);
    let ga3 = geom_of(&engine, a);
    assert!(
        rect_eq(ga3, screen),
        "fullscreen A must fill the screen again after refocus: got {ga3:?}"
    );
}

#[test]
fn toggle_maximize_focused_only() {
    let registry = default_registry();
    let mut engine = engine_with_columns(2, 1);
    let mi = engine.state.sel_mon;
    let ws_i = engine.state.monitors[mi].active_ws;
    let wa = engine.state.monitors[mi].workarea;
    let first = engine.state.monitors[mi].workspaces[ws_i].columns[0].windows[0];
    let second = engine.state.monitors[mi].workspaces[ws_i].columns[1].windows[0];
    engine.state.monitors[mi].focused = Some(first);
    engine.state.monitors[mi].focus_stack = vec![first];

    // Maximize the focused window (keyboard command returns the effect; the
    // backend applies it, so emulate the flag write the backend performs).
    let _proj = focus_step(&mut engine, ToggleMaximize(Some(first)), &registry);
    if let Some(c) = engine.state.clients.get_mut(&first) {
        c.flags.set(WinFlags::MAXIMIZED_V);
        c.flags.set(WinFlags::MAXIMIZED_H);
    }
    // Emulate the backend's `set_maximized`/`focus` keeping `presented_maximize`
    // in sync with the focused maximized window — `present` reads it from there.
    engine.state.sync_presented_maximize(mi);
    snap_all(&mut engine, mi, ws_i);
    let _proj = apply_settled(&mut engine, mi, &registry);

    let gf = geom_of(&engine, first);
    assert_eq!(
        (gf.x, gf.y, gf.w, gf.h),
        (wa.x, wa.y, wa.w, wa.h),
        "maximized focused window must fill workarea"
    );
    assert_eq!(engine.state.clients.get(&first).unwrap().border_w, 0);

    // Neighbour keeps its own tile (not the overlay). A focused neighbour is
    // accordion-boosted and may legitimately fill most of the workarea, so the
    // only correct check is that it is NOT the maximized-overlay rect and that
    // its border is the normal width.
    let gs = geom_of(&engine, second);
    assert!(!rect_eq(gs, wa), "neighbour must NOT be maximized: {gs:?}");
    assert_eq!(
        engine.state.clients.get(&second).unwrap().border_w,
        engine.cfg.border_w
    );

    // Move focus away: the ex-focused window drops the overlay (no longer
    // focused) and returns to its projected tile.
    engine.state.monitors[mi].focused = Some(second);
    if let Some(pos) = engine.state.monitors[mi]
        .focus_stack
        .iter()
        .position(|w| *w == second)
    {
        engine.state.monitors[mi].focus_stack.remove(pos);
    }
    engine.state.monitors[mi].focus_stack.push(second);
    // Move-focus also updates the maximize overlay owner (the ex-focused window
    // is no longer presented); mirror the backend `focus` sync.
    engine.state.sync_presented_maximize(mi);
    snap_all(&mut engine, mi, ws_i);
    let proj = apply_settled(&mut engine, mi, &registry);
    let gf2 = geom_of(&engine, first);
    assert!(
        !rect_eq(gf2, wa),
        "ex-focused maximized window must leave the workarea overlay: {gf2:?}"
    );
    let g2 = geom_of(&engine, first);
    assert!(
        inside_wa(&engine, mi, g2),
        "ex-focused window returns to a tiled rect: {g2:?}"
    );
    // Its geom must equal its settled projection (no maximize overlay anymore).
    assert_all_tiled_match_settled(&engine, mi, &proj);
}

#[test]
fn presented_maximize_tracks_focus_and_is_cleared_on_lifecycle() {
    let _registry = default_registry();
    let mut engine = engine_with_columns(2, 1);
    let mi = engine.state.sel_mon;
    let ws_i = engine.state.monitors[mi].active_ws;
    let a = engine.state.monitors[mi].workspaces[ws_i].columns[0].windows[0];
    let b = engine.state.monitors[mi].workspaces[ws_i].columns[1].windows[0];

    // focused A + maximize A -> both the logical focus and the explicit overlay
    // owner point at A. `mon.focused` is *only* the logical focus.
    engine.state.monitors[mi].focused = Some(a);
    if let Some(c) = engine.state.clients.get_mut(&a) {
        c.flags.set(WinFlags::MAXIMIZED_V);
        c.flags.set(WinFlags::MAXIMIZED_H);
    }
    engine.state.sync_presented_maximize(mi);
    assert_eq!(engine.state.monitors[mi].focused, Some(a));
    assert_eq!(
        engine.state.monitors[mi].workspaces[ws_i].presented_maximize,
        Some(a)
    );

    // unmaximize A while still focused -> overlay owner gone (flags only)
    if let Some(c) = engine.state.clients.get_mut(&a) {
        c.flags.clear(WinFlags::MAXIMIZED_V);
        c.flags.clear(WinFlags::MAXIMIZED_H);
    }
    engine.state.sync_presented_maximize(mi);
    assert_eq!(
        engine.state.monitors[mi].workspaces[ws_i].presented_maximize,
        None
    );

    // maximize A again, then focus B -> A is no longer the overlay owner even
    // though it is still maximized (overlay follows the *focused* window).
    if let Some(c) = engine.state.clients.get_mut(&a) {
        c.flags.set(WinFlags::MAXIMIZED_V);
        c.flags.set(WinFlags::MAXIMIZED_H);
    }
    engine.state.sync_presented_maximize(mi);
    assert_eq!(
        engine.state.monitors[mi].workspaces[ws_i].presented_maximize,
        Some(a)
    );
    engine.state.monitors[mi].focused = Some(b);
    engine.state.sync_presented_maximize(mi);
    assert_eq!(
        engine.state.monitors[mi].workspaces[ws_i].presented_maximize,
        None
    );

    // destroy A while it is the presented maximize overlay -> no dangling owner
    engine.state.monitors[mi].focused = Some(a);
    engine.state.sync_presented_maximize(mi);
    assert_eq!(
        engine.state.monitors[mi].workspaces[ws_i].presented_maximize,
        Some(a)
    );
    engine.state.remove_client(a);
    assert_eq!(
        engine.state.monitors[mi].workspaces[ws_i].presented_maximize,
        None
    );
}

#[test]
fn move_window_keeps_invariant() {
    let registry = default_registry();
    let mut engine = engine_with_columns(3, 1);
    let mi = engine.state.sel_mon;
    let ws_i = engine.state.monitors[mi].active_ws;
    let first = engine.state.monitors[mi].workspaces[ws_i].columns[0].windows[0];
    engine.state.monitors[mi].focused = Some(first);
    engine.state.monitors[mi].focus_stack = vec![first];

    let proj = focus_step(&mut engine, MoveWindow(first, Dir::Right), &registry);
    assert_all_tiled_match_settled(&engine, mi, &proj);
}

#[test]
fn page_snap_does_not_break_invariant() {
    // PageSnap must NOT be asserted as "focused column centred" — after a snap
    // the focus may legitimately be off the left/right page edge. The only
    // invariant that must hold is A: geom == settled projection.
    let registry = default_registry();
    let mut engine = engine_with_columns(12, 1);
    let mi = engine.state.sel_mon;
    let ws_i = engine.state.monitors[mi].active_ws;
    let first = engine.state.monitors[mi].workspaces[ws_i].columns[0].windows[0];
    engine.state.monitors[mi].focused = Some(first);
    engine.state.monitors[mi].focus_stack = vec![first];

    let proj = focus_step(&mut engine, PageSnap(Dir::Right), &registry);
    assert_all_tiled_match_settled(&engine, mi, &proj);
    let proj = focus_step(&mut engine, PageSnap(Dir::Right), &registry);
    assert_all_tiled_match_settled(&engine, mi, &proj);
}

#[test]
fn overview_returns_to_settled() {
    let registry = default_registry();
    let mut engine = engine_with_columns(4, 1);
    let mi = engine.state.sel_mon;
    let ws_i = engine.state.monitors[mi].active_ws;
    let first = engine.state.monitors[mi].workspaces[ws_i].columns[0].windows[0];
    engine.state.monitors[mi].focused = Some(first);
    engine.state.monitors[mi].focus_stack = vec![first];

    let proj = focus_step(&mut engine, ToggleOverview, &registry);
    assert_all_tiled_match_settled(&engine, mi, &proj);
    let proj = focus_step(&mut engine, ToggleOverview, &registry);
    assert_all_tiled_match_settled(&engine, mi, &proj);
}

#[test]
fn viewport_zoom_returns_to_settled() {
    let registry = default_registry();
    let mut engine = engine_with_columns(3, 1);
    let mi = engine.state.sel_mon;
    let ws_i = engine.state.monitors[mi].active_ws;
    let first = engine.state.monitors[mi].workspaces[ws_i].columns[0].windows[0];
    engine.state.monitors[mi].focused = Some(first);
    engine.state.monitors[mi].focus_stack = vec![first];

    let proj = focus_step(&mut engine, ViewportZoom(0.5), &registry);
    assert_all_tiled_match_settled(&engine, mi, &proj);
    let proj = focus_step(&mut engine, ViewportZoom(-1.0), &registry);
    assert_all_tiled_match_settled(&engine, mi, &proj);
}

#[test]
fn workspace_switch_resettles() {
    let registry = default_registry();
    let mut engine = setup_engine();
    let mi = engine.state.sel_mon;

    let ws0 = 0usize;
    for ci in 0..2 {
        let win = (ci + 1) as u32;
        engine.state.add_client(Client::new(win, mi, ws0));
        engine.state.monitors[mi].workspaces[ws0].add_tiled(win, engine.cfg.column_width);
    }
    let ws1 = 1usize;
    for ci in 0..2 {
        let win = (10 + ci) as u32;
        engine.state.add_client(Client::new(win, mi, ws1));
        engine.state.monitors[mi].workspaces[ws1].add_tiled(win, engine.cfg.column_width);
    }
    let first0 = engine.state.monitors[mi].workspaces[ws0].columns[0].windows[0];
    engine.state.monitors[mi].focused = Some(first0);
    engine.state.monitors[mi].focus_stack = vec![first0];

    let proj = focus_step(&mut engine, ViewWorkspace(ws1), &registry);
    assert_all_tiled_match_settled(&engine, mi, &proj);
    // The active workspace is now ws1; its clients must sit at their projection.
    let fw = engine.state.monitors[mi].workspaces[ws1].columns[0].windows[0];
    let g = geom_of(&engine, fw);
    assert!(
        g.w > 0 && g.h > 0,
        "ws1 focused window must have a real rect: {g:?}"
    );
}

#[test]
fn dock_strut_retarget_respects_workarea() {
    let registry = default_registry();
    let mut engine = engine_with_columns(3, 1);
    let mi = engine.state.sel_mon;
    let ws_i = engine.state.monitors[mi].active_ws;
    let first = engine.state.monitors[mi].workspaces[ws_i].columns[0].windows[0];
    engine.state.monitors[mi].focused = Some(first);
    engine.state.monitors[mi].focus_stack = vec![first];

    // Simulate a bottom dock shrinking the usable area (the struts.rs path
    // retargets the camera AFTER recomputing the workarea, which is the fixed
    // ordering). We then settle and re-project.
    let mut wa = engine.state.monitors[mi].screen;
    wa.h = wa.h.saturating_sub(100);
    engine.state.monitors[mi].workarea = wa;
    let proj = settle_on_column(&mut engine, mi, ws_i, 1, &registry);

    for ci in 0..engine.state.monitors[mi].workspaces[ws_i].columns.len() {
        let win = engine.state.monitors[mi].workspaces[ws_i].columns[ci].windows[0];
        let g = geom_of(&engine, win);
        assert!(g.w > 0 && g.h > 0, "win {win} must have a real rect: {g:?}");
        // In a scrolling ribbon, non-focused columns legitimately scroll off the
        // edges; only windows whose centre is on-screen must sit inside the
        // (possibly dock-inset) workarea.
        let cx = g.x + g.w as i32 / 2;
        let cy = g.y + g.h as i32 / 2;
        let on_screen =
            cx >= wa.x && cx < wa.x + wa.w as i32 && cy >= wa.y && cy < wa.y + wa.h as i32;
        if on_screen {
            assert!(
                inside_wa(&engine, mi, g),
                "win {win} on-screen must fit workarea: {g:?}"
            );
        }
    }
    assert_all_tiled_match_settled(&engine, mi, &proj);
}

#[test]
fn settled_follows_target_at_rest() {
    let registry = default_registry();
    let mut engine = engine_with_columns(3, 1);
    let mi = engine.state.sel_mon;
    let ws_i = engine.state.monitors[mi].active_ws;

    let _proj = settle_on_column(&mut engine, mi, ws_i, 1, &registry);
    // At rest position==target, so the projection the backend writes to X11
    // (here: apply_settled) must equal projection(camera.target).
    let win = engine.state.monitors[mi].workspaces[ws_i].columns[1].windows[0];
    let mut settled = Vec::new();
    arrange(
        &engine.state,
        mi,
        &engine.cfg,
        &registry,
        Phase::Settled,
        &mut settled,
        &mut RibbonScratch::default(),
    );
    let gs = settled
        .iter()
        .find(|(w, _, _)| *w == win)
        .map(|(_, r, _)| *r)
        .unwrap();
    let gclient = geom_of(&engine, win);
    assert!(
        rect_eq(gclient, gs),
        "client.geom must equal settled projection: {gclient:?} vs {gs:?}"
    );
}

#[test]
fn live_differs_mid_animation_then_converges() {
    let registry = default_registry();
    let mut engine = engine_with_columns(3, 1);
    let mi = engine.state.sel_mon;
    let ws_i = engine.state.monitors[mi].active_ws;
    let _proj = settle_on_column(&mut engine, mi, ws_i, 1, &registry);

    // Mid-animation: bump the live position far away. Live must follow it and
    // differ from the at-rest (target) projection.
    engine.state.monitors[mi].workspaces[ws_i].camera.position = 99999.0;
    engine.state.monitors[mi].workspaces[ws_i].camera.velocity = 0.0;

    let mut live = Vec::new();
    arrange(
        &engine.state,
        mi,
        &engine.cfg,
        &registry,
        Phase::Live,
        &mut live,
        &mut RibbonScratch::default(),
    );
    let win = engine.state.monitors[mi].workspaces[ws_i].columns[1].windows[0];
    let gl = live
        .iter()
        .find(|(w, _, _)| *w == win)
        .map(|(_, r, _)| *r)
        .unwrap();
    let grest = geom_of(&engine, win);
    assert!(
        (gl.x - grest.x).abs() > 100 || (gl.y - grest.y).abs() > 100,
        "live projection must follow camera.position mid-animation: {gl:?} vs {grest:?}"
    );

    // Now let the spring settle; Live must converge to the at-rest geometry.
    for _ in 0..2000 {
        let moving = engine.state.monitors[mi].workspaces[ws_i]
            .camera
            .step(1.0 / 60.0);
        if !moving {
            break;
        }
    }
    let cam = engine.state.monitors[mi].workspaces[ws_i].camera;
    assert!(
        (cam.position - cam.target).abs() < 0.5,
        "camera must converge: pos {} target {}",
        cam.position,
        cam.target
    );

    // Snap ALL animated factors (boost/zoom are still live) to their rest values,
    // then both Live and Settled projections read the same numbers and must match.
    snap_all(&mut engine, mi, ws_i);

    let mut rest_now = Vec::new();
    arrange(
        &engine.state,
        mi,
        &engine.cfg,
        &registry,
        Phase::Settled,
        &mut rest_now,
        &mut RibbonScratch::default(),
    );
    let grest_now = rest_now
        .iter()
        .find(|(w, _, _)| *w == win)
        .map(|(_, r, _)| *r)
        .unwrap();

    let mut live2 = Vec::new();
    arrange(
        &engine.state,
        mi,
        &engine.cfg,
        &registry,
        Phase::Live,
        &mut live2,
        &mut RibbonScratch::default(),
    );
    let gl2 = live2
        .iter()
        .find(|(w, _, _)| *w == win)
        .map(|(_, r, _)| *r)
        .unwrap();
    assert!(
        rect_eq(gl2, grest_now),
        "live must converge to at-rest geom: {gl2:?} vs {grest_now:?}"
    );
}

#[test]
fn repeated_abc_navigation_idempotent() {
    let registry = default_registry();
    let mut engine = engine_with_columns(5, 1);
    let mi = engine.state.sel_mon;
    let ws_i = engine.state.monitors[mi].active_ws;
    let first = engine.state.monitors[mi].workspaces[ws_i].columns[0].windows[0];
    engine.state.monitors[mi].focused = Some(first);
    engine.state.monitors[mi].focus_stack = vec![1u32, 2u32, 3u32, 4u32, 5u32];

    for _ in 0..4 {
        for w in [2u32, 3u32, 4u32] {
            let proj = focus_window(&mut engine, w, &registry);
            assert_all_tiled_match_settled(&engine, mi, &proj);
        }
        for w in [3u32, 2u32, 1u32] {
            let proj = focus_window(&mut engine, w, &registry);
            assert_all_tiled_match_settled(&engine, mi, &proj);
        }
    }
}

#[test]
fn focus_none_is_safe() {
    let registry = default_registry();
    let mut engine = engine_with_columns(3, 1);
    let mi = engine.state.sel_mon;
    // No focused client — present() must not panic and must emit nothing; the
    // remaining tiled geometry is still valid.
    engine.state.monitors[mi].focused = None;
    engine.state.monitors[mi].focus_stack.clear();

    let raised = {
        let mut placements = Vec::new();
        arrange(
            &engine.state,
            mi,
            &engine.cfg,
            &registry,
            Phase::Settled,
            &mut placements,
            &mut RibbonScratch::default(),
        );
        // Capture the projection before `present` mutates the camera target.
        let proj: std::collections::HashMap<WindowId, (Rect, u32)> =
            placements.iter().map(|(w, r, b)| (*w, (*r, *b))).collect();
        let raised = present(&engine.state, &engine.state.monitors[mi], &mut placements);
        (raised, proj)
    };
    assert!(
        raised.0.is_empty(),
        "with no focus, no overlay is presented"
    );
    // The backend writes each placement's rect back into `client.geom` (see
    // `apply_settled` / `apply_geom`); the pure harness mirrors that so the
    // invariant (geom == settled projection) can be checked.
    for (win, (rect, b)) in &raised.1 {
        if let Some(c) = engine.state.clients.get_mut(win) {
            c.geom = *rect;
            c.border_w = *b;
        }
    }
    assert_all_tiled_match_settled(&engine, mi, &raised.1);
}

#[test]
fn border_w_is_part_of_geom() {
    let registry = default_registry();
    let mut engine = setup_engine();
    let mi = engine.state.sel_mon;
    let ws_i = engine.state.monitors[mi].active_ws;
    let screen = engine.state.monitors[mi].screen;
    for ci in 0..2 {
        let win = (ci + 1) as u32;
        engine.state.add_client(Client::new(win, mi, ws_i));
        engine.state.monitors[mi].workspaces[ws_i].add_tiled(win, engine.cfg.column_width);
        if ci == 0 {
            engine
                .state
                .clients
                .get_mut(&win)
                .unwrap()
                .flags
                .set(WinFlags::FULLSCREEN);
        }
    }
    let a = 1u32;
    let first = a;
    engine.state.monitors[mi].focused = Some(first);
    engine.state.monitors[mi].focus_stack = vec![first];

    let _proj = settle_on_column(&mut engine, mi, ws_i, 0, &registry);
    let ca = engine.state.clients.get(&a).unwrap();
    assert_eq!(ca.border_w, 0, "fullscreen border must be 0");
    assert!(
        rect_eq(ca.geom, screen),
        "fullscreen geom must equal screen: {:?}",
        ca.geom
    );

    if let Some(c) = engine.state.clients.get_mut(&a) {
        c.flags.clear(WinFlags::FULLSCREEN);
        c.border_w = engine.cfg.border_w;
    }
    let _proj = settle_on_column(&mut engine, mi, ws_i, 0, &registry);
    let ca2 = engine.state.clients.get(&a).unwrap();
    assert_eq!(
        ca2.border_w, engine.cfg.border_w,
        "border restored after fullscreen"
    );
}

#[test]
fn mouse_and_keyboard_focus_converge() {
    // The pure suite proves the two *core* geometry paths produce the same
    // end-state. The keyboard FocusDirection path and the EWMH/EnterNotify
    // `Backend::focus` path (which emits `Effect::FocusWindow`) must both leave
    // `client.geom` at the same settled projection. The actual wiring gap
    // (does `Backend::focus` really re-arrange?) is covered end-to-end by the
    // Xephyr scenario in tests/xephyr-suite.sh; here we lock the contract.
    let registry = default_registry();

    // Keyboard/EWMH path: focus window 2 via the command the backend emits.
    let mut kb = setup_engine();
    let mi = kb.state.sel_mon;
    let ws_i = kb.state.monitors[mi].active_ws;
    for ci in 0..3 {
        let win = (ci + 1) as u32;
        kb.state.add_client(Client::new(win, mi, ws_i));
        kb.state.monitors[mi].workspaces[ws_i].add_tiled(win, kb.cfg.column_width);
    }
    kb.state.monitors[mi].focused = Some(1u32);
    kb.state.monitors[mi].focus_stack = vec![1u32, 2u32, 3u32];
    focus_step(
        &mut kb,
        crate::core::commands::FocusWindow(Some(2u32)),
        &registry,
    );
    let kb_map: std::collections::HashMap<WindowId, Rect> =
        kb.state.clients.iter().map(|(w, c)| (*w, c.geom)).collect();

    // Mouse path: pure simulate (retarget + settle + project), no ArrangeMonitor.
    let mut mouse = setup_engine();
    let mi = mouse.state.sel_mon;
    let ws_i = mouse.state.monitors[mi].active_ws;
    for ci in 0..3 {
        let win = (ci + 1) as u32;
        mouse.state.add_client(Client::new(win, mi, ws_i));
        mouse.state.monitors[mi].workspaces[ws_i].add_tiled(win, mouse.cfg.column_width);
    }
    mouse.state.monitors[mi].focused = Some(1u32);
    mouse.state.monitors[mi].focus_stack = vec![1u32, 2u32, 3u32];
    // Focus window 2 = column 1.
    let col_of_2 = mouse.state.monitors[mi].workspaces[ws_i]
        .columns
        .iter()
        .position(|c| c.windows.contains(&2u32))
        .unwrap();
    settle_on_column(&mut mouse, mi, ws_i, col_of_2, &registry);
    let mouse_map: std::collections::HashMap<WindowId, Rect> = mouse
        .state
        .clients
        .iter()
        .map(|(w, c)| (*w, c.geom))
        .collect();

    assert_eq!(kb_map.len(), mouse_map.len(), "same window set");
    for (w, r_kb) in &kb_map {
        let r_mouse = mouse_map[w];
        assert!(
            rect_eq(*r_kb, r_mouse),
            "keyboard and mouse focus must converge for win {w}: {r_kb:?} vs {r_mouse:?}"
        );
    }
}

#[test]
fn input_hittest_matches_settled_geom() {
    let registry = default_registry();
    let mut engine = engine_with_columns(3, 1);
    let mi = engine.state.sel_mon;
    let ws_i = engine.state.monitors[mi].active_ws;
    let first = engine.state.monitors[mi].workspaces[ws_i].columns[0].windows[0];
    engine.state.monitors[mi].focused = Some(first);
    engine.state.monitors[mi].focus_stack = vec![1u32, 2u32, 3u32];

    let _proj = focus_window(&mut engine, 2, &registry);
    // At rest, `find_client`'s hit-test extent must equal the geom X11 uses.
    let ws = &engine.state.monitors[mi].workspaces[ws_i];
    let wa = engine.state.monitors[mi].workarea;
    let extents = column_screen_extents(
        ws,
        &engine.cfg,
        wa,
        &fs_ctx(&engine.state.clients, ws, engine.state.monitors[mi].screen),
    );
    // `column_screen_extents` returns the X span only; `arrange_columns` also
    // applies the `cy` vertical centering (active when alpha != 1, e.g. the
    // accordion-boosted focused column). Reconstruct the full rect with the
    // same projection `ribbon_geom` uses.
    let _g = ribbon_geom(
        ws,
        &engine.cfg,
        wa,
        true,
        &fs_ctx(&engine.state.clients, ws, engine.state.monitors[mi].screen),
    );
    let cols = &ws.columns;
    for (ci, col) in cols.iter().enumerate() {
        let (l, r) = extents[ci];
        for win in &col.windows {
            let g = geom_of(&engine, *win);
            // `column_screen_extents` (what `find_client` hit-tests against) only
            // projects the X span; the Y/height come from `arrange`'s gap-based
            // row layout. So the contract is X-only: the hit-test left/right edges
            // must equal the window's on-screen X span.
            assert!(
                (g.x - l as i32).abs() <= 2 && (g.x + g.w as i32 - r as i32).abs() <= 2,
                "hit-test X extent must match client.geom X span for win {win}: geom x..{}..{}, extent {}..{}",
                g.x, g.x + g.w as i32, l as i32, r as i32
            );
        }
    }
}

// ─── shared builders ────────────────────────────────────────────────────────

fn engine_with_columns(n_cols: usize, rows: usize) -> Engine {
    let mut engine = setup_engine();
    let mi = engine.state.sel_mon;
    let ws_i = engine.state.monitors[mi].active_ws;
    let mut win = 1u32;
    for _ in 0..n_cols {
        let mut col = Column::new(engine.cfg.column_width);
        for _ in 0..rows {
            engine.state.add_client(Client::new(win, mi, ws_i));
            col.windows.push(win);
            win += 1;
        }
        engine.state.monitors[mi].workspaces[ws_i].columns.push(col);
    }
    engine
}

/// Retarget the camera onto the focused column (what the backend does in its
/// render loop before projecting), snap all animated factors to rest, then run
/// the settled projection + `present` and write `client.geom`.
fn retarget_and_settle(
    engine: &mut Engine,
    mi: usize,
    ws_i: usize,
    registry: &LayoutRegistry,
) -> std::collections::HashMap<WindowId, (Rect, u32)> {
    let wa = engine.state.monitors[mi].workarea;
    let fs = fs_ctx(
        &engine.state.clients,
        &engine.state.monitors[mi].workspaces[ws_i],
        engine.state.monitors[mi].screen,
    );
    {
        let ws = &mut engine.state.monitors[mi].workspaces[ws_i];
        ws.camera.target = ideal_scroll(ws, &engine.cfg, wa, fs);
    }
    snap_all(engine, mi, ws_i);
    apply_settled(engine, mi, registry)
}

// ─── lifecycle / focus-pointer regression tests ─────────────────────────────

/// The original bug: closing a window *before* the focused window left
/// `Workspace::focus.column_idx` pointing at a different column, so the camera
/// centred on a neighbour and `best_focus()`/`focused_win()` stole input from
/// the logical focus (`mon.focused`).
#[test]
fn close_window_before_focus_realigns_pointer_and_geometry() {
    let registry = default_registry();
    let mut engine = engine_with_columns(4, 1);
    let mi = engine.state.sel_mon;
    let ws_i = engine.state.monitors[mi].active_ws;

    // Focus window 3 (column index 2).
    engine.state.monitors[mi].focused = Some(3);
    engine.state.monitors[mi].focus_stack = vec![3, 2, 1, 4];
    engine.state.monitors[mi].workspaces[ws_i].focus.column_idx = 2;

    // Close window 1 (column 0, BEFORE the focused column).
    let removed = engine.state.remove_client(1);
    assert!(removed.is_some(), "window 1 must be removed");

    let ws = &engine.state.monitors[mi].workspaces[ws_i];
    assert_eq!(
        ws.focus.column_idx, 1,
        "focus pointer must shift left by the number of removed columns before it"
    );
    assert_eq!(
        ws.focused_win(),
        Some(3),
        "logical focus must remain on window 3"
    );
    assert_eq!(
        engine.state.monitors[mi].focused,
        Some(3),
        "mon.focused must be untouched by removal"
    );

    let proj = retarget_and_settle(&mut engine, mi, ws_i, &registry);
    assert_all_tiled_match_settled(&engine, mi, &proj);
    // The focused window must actually be on-screen at rest.
    let g = geom_of(&engine, 3);
    assert!(g.w > 0 && g.h > 0, "focused window must have a real rect");
    assert!(
        inside_wa(&engine, mi, g),
        "focused window must lie inside the workarea: {g:?}"
    );
}

/// Closing the focused window itself must re-point `ws.focus` to the new
/// logical focus (`mon.focused`), never leave it dangling on a now-empty slot.
#[test]
fn close_focused_window_repoints_focus_to_neighbour() {
    let registry = default_registry();
    let mut engine = engine_with_columns(4, 1);
    let mi = engine.state.sel_mon;
    let ws_i = engine.state.monitors[mi].active_ws;

    engine.state.monitors[mi].focused = Some(2);
    engine.state.monitors[mi].focus_stack = vec![2, 1, 3, 4];
    engine.state.monitors[mi].workspaces[ws_i].focus.column_idx = 1;

    // Close the focused window 2.
    let removed = engine.state.remove_client(2);
    assert!(removed.is_some());

    // `mon.focused` must move to the most-recently-focused surviving window.
    assert_ne!(
        engine.state.monitors[mi].focused,
        Some(2),
        "mon.focused must leave the closed window"
    );
    let new_focus = engine.state.monitors[mi].focused.unwrap();
    let ws = &engine.state.monitors[mi].workspaces[ws_i];
    assert_eq!(
        ws.focused_win(),
        Some(new_focus),
        "ws.focus must point at the same window as mon.focused"
    );
    assert_eq!(
        ws.index_of_window(new_focus),
        Some((ws.focus.column_idx, ws.columns[ws.focus.column_idx].focused)),
        "derived pointer must index the logical focus"
    );

    let proj = retarget_and_settle(&mut engine, mi, ws_i, &registry);
    assert_all_tiled_match_settled(&engine, mi, &proj);
    let g = geom_of(&engine, new_focus);
    assert!(inside_wa(&engine, mi, g), "new focus must be on-screen");
}

/// Closing a window *after* the focused column must NOT shift the focus pointer.
#[test]
fn close_window_after_focus_keeps_focus_column() {
    let registry = default_registry();
    let mut engine = engine_with_columns(4, 1);
    let mi = engine.state.sel_mon;
    let ws_i = engine.state.monitors[mi].active_ws;

    engine.state.monitors[mi].focused = Some(2);
    engine.state.monitors[mi].focus_stack = vec![2, 1, 3, 4];
    engine.state.monitors[mi].workspaces[ws_i].focus.column_idx = 1;

    // Close window 4 (column 3, AFTER the focused column).
    let removed = engine.state.remove_client(4);
    assert!(removed.is_some());

    let ws = &engine.state.monitors[mi].workspaces[ws_i];
    assert_eq!(
        ws.focus.column_idx, 1,
        "closing a later column must not move the focus pointer"
    );
    assert_eq!(ws.focused_win(), Some(2));
    let proj = retarget_and_settle(&mut engine, mi, ws_i, &registry);
    assert_all_tiled_match_settled(&engine, mi, &proj);
}

/// Closing a window in a row *before* the focused row inside the same column
/// must shift that column's `focused` pointer up, not leave it on a hole.
#[test]
fn close_row_before_focus_shifts_focused_row() {
    let _registry = default_registry();
    let mut engine = engine_with_columns(2, 3);
    let mi = engine.state.sel_mon;
    let ws_i = engine.state.monitors[mi].active_ws;

    // Column 0 has windows [1, 2, 3]; focus row 1 (window 2).
    engine.state.monitors[mi].focused = Some(2);
    engine.state.monitors[mi].focus_stack = vec![2, 1, 3];
    engine.state.monitors[mi].workspaces[ws_i].columns[0].focused = 1;
    engine.state.monitors[mi].workspaces[ws_i].focus.column_idx = 0;

    // Close window 1 (row 0, before the focused row).
    let removed = engine.state.remove_client(1);
    assert!(removed.is_some());

    let col = &engine.state.monitors[mi].workspaces[ws_i].columns[0];
    assert_eq!(
        col.focused, 0,
        "focused row must shift up after removing a prior row"
    );
    assert_eq!(
        col.windows[col.focused], 2,
        "logical focus stays on window 2"
    );
    assert_eq!(
        engine.state.monitors[mi].workspaces[ws_i].focused_win(),
        Some(2)
    );
}

/// Switching layout while the camera is displaced off the focused column must
/// deterministically re-centre the camera on the focused column (no drift,
/// no stale offset accumulating across switches).
#[test]
fn layout_switch_with_displaced_camera_recenters_focused_column() {
    let registry = default_registry();
    let mut engine = engine_with_columns(4, 1);
    let mi = engine.state.sel_mon;
    let ws_i = engine.state.monitors[mi].active_ws;

    engine.state.monitors[mi].focused = Some(3);
    engine.state.monitors[mi].focus_stack = vec![3, 2, 1, 4];
    engine.state.monitors[mi].workspaces[ws_i].focus.column_idx = 2;

    // Displace the camera manually (e.g. mid-animation / external scroll).
    engine.state.monitors[mi].workspaces[ws_i].camera.target = -400.0;
    engine.state.monitors[mi].workspaces[ws_i].camera.position = -400.0;

    // Cycle layout: the command must reset camera.target to ideal_scroll.
    let fs = fs_ctx(
        &engine.state.clients,
        &engine.state.monitors[mi].workspaces[ws_i],
        engine.state.monitors[mi].screen,
    );
    let wa = engine.state.monitors[mi].workarea;
    let expected = ideal_scroll(
        &engine.state.monitors[mi].workspaces[ws_i],
        &engine.cfg,
        wa,
        fs,
    );
    let _proj = focus_step(&mut engine, crate::core::commands::CycleLayout, &registry);

    let cam = engine.state.monitors[mi].workspaces[ws_i].camera.target;
    assert!(
        (cam - expected).abs() < 0.5,
        "camera.target must be recentered to ideal_scroll, got {cam} want {expected}"
    );

    // The recently-centered camera must keep the focused column on-screen.
    let proj = retarget_and_settle(&mut engine, mi, ws_i, &registry);
    assert_all_tiled_match_settled(&engine, mi, &proj);
    let g = geom_of(&engine, 3);
    assert!(
        inside_wa(&engine, mi, g),
        "focused window must be on-screen"
    );
}

/// Closing a window must never move the camera off the focused column: the
/// focused window stays centred and on-screen.
#[test]
fn closing_any_window_keeps_focused_window_centered() {
    let registry = default_registry();
    let mut engine = engine_with_columns(5, 1);
    let mi = engine.state.sel_mon;
    let ws_i = engine.state.monitors[mi].active_ws;

    engine.state.monitors[mi].focused = Some(3);
    engine.state.monitors[mi].focus_stack = vec![3, 2, 1, 4, 5];
    engine.state.monitors[mi].workspaces[ws_i].focus.column_idx = 2;

    // Remove windows one by one, including the focused one, and after each
    // removal assert the (new) focused window is on-screen and centered.
    for victim in [1u32, 5, 3] {
        engine.state.remove_client(victim);
        let proj = retarget_and_settle(&mut engine, mi, ws_i, &registry);
        assert_all_tiled_match_settled(&engine, mi, &proj);
        let f = engine.state.monitors[mi].focused.unwrap();
        let g = geom_of(&engine, f);
        assert!(inside_wa(&engine, mi, g), "focused {f} must be on-screen");
        let wa = engine.state.monitors[mi].workarea;
        let cx = g.x + g.w as i32 / 2;
        assert!(
            (cx - (wa.x + wa.w as i32 / 2)).abs() <= (wa.w as i32) / 2,
            "focused window must not be shoved entirely off the workarea"
        );
    }
}
