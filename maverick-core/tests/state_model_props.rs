//! Properties of the core domain model: the client/window/column tree, the focus
//! pointers into it, the flag and hint words, and `State::check_invariants`.
//!
//! `check_invariants` is the WM's own model checker, called by `Engine::execute`
//! after every command in a debug build. Two properties matter for it and both
//! are asserted here: a state assembled through the public API must satisfy it,
//! and a state with exactly one field corrupted must *not* — otherwise the
//! checker is either useless noise, or a set of checks that silently stopped
//! firing.

mod common;

use common::{arb_dir, arb_screen, arb_spec, build, focus_logically, Built};
use maverick_core::types::{
    Client, Column, Dir, FullscreenPolicy, Monitor, PendingFocus, Rect, SizeHints, State, WinFlags,
    WindowId, Workspace,
};
use proptest::prelude::*;

// The two `XSizeHints.flags` bits that carry a position claim, per ICCCM 4.1.2.3.
const POSITION_BITS: u32 = SizeHints::U_S_POSITION | SizeHints::P_POSITION;

// --- a monitor is usable from its constructor -------------------------------
//
// `Monitor::ws` / `ws_mut` are how every command path reaches a monitor's active
// workspace, so a constructor that can leave a monitor with no workspace at all
// turns a tag count of zero into a panic on the first command rather than into a
// monitor. The model checker is the second, independent witness: a monitor with
// no slots is not a well-formed state either.
proptest! {
    #[test]
    fn a_monitor_is_usable_from_its_constructor_at_any_tag_count(
        screen in arb_screen(),
        n_tags in 0usize..=12,
    ) {
        let mut st = State::new();
        st.monitors.push(Monitor::new(screen, n_tags));

        // At least one slot must exist, tagged `0`, whatever was asked for.
        prop_assert!(
            !st.monitors[0].workspaces.is_empty(),
            "a monitor built for {n_tags} tags has no workspace to work on"
        );
        let tag = st.monitors[0].ws_mut().tag;
        prop_assert_eq!(st.monitors[0].ws().tag, tag);
        prop_assert_eq!(st.monitors[0].active_ws, 0);

        prop_assert!(
            st.check_invariants().is_ok(),
            "a monitor built for {n_tags} tags is not a well-formed state: {:?}",
            st.check_invariants()
        );
    }
}

// --- the model checker as an oracle -----------------------------------------

// A state the WM could actually be in satisfies every documented invariant.
//
// The fixture only uses the public transition API (`add_client`, `add_tiled`,
// `drop_into_column`, `set_reserved_region`, `sync_presented_maximize`,
// `reconcile_workspaces`), so a violation can only come from the checker
// rejecting a state the WM genuinely produces — for example by tightening a
// check to demand that every managed client be placed in the tree, which the
// checker documents as *not* required (mid-manage windows and test scaffolding
// are unplaced by design).
proptest! {
    #[test]
    fn states_built_through_the_public_api_satisfy_every_invariant(spec in arb_spec()) {
        let built = build(&spec);
        let checked = built.state.check_invariants();
        prop_assert!(checked.is_ok(), "a state built through the public API is broken: {:?}", checked);
        #[cfg(debug_assertions)]
        built.state.assert_invariants();
    }
}

// Every single-field corruption is reported, by the check that owns it.
//
// This is the other half of the oracle: a check that stopped firing (a deleted
// clause, a renamed binding, an `&&` turned into `||`) would leave the matching
// corruption silent and the WM would corrupt its model in a debug build without a
// complaint. Each case asserts the specific clause rather than "some violation",
// so a neighbouring check cannot stand in for it.
proptest! {
    #[test]
    fn every_single_field_corruption_is_reported_by_its_own_check(
        spec in arb_spec(),
        violation in arb_violation(),
    ) {
        let Built { mut state, .. } = build(&spec);
        let before = state.check_invariants();
        prop_assert!(before.is_ok(), "the fixture state is already broken: {:?}", before);

        let expected = violation.apply(&mut state);
        let after = state.check_invariants();
        let Err(violations) = after else {
            prop_assert!(false, "{} went unreported by check_invariants", expected);
            return Ok(());
        };
        prop_assert!(
            violations.iter().any(|v| v.contains(expected)),
            "{} went unreported; the checker said {:?}",
            expected,
            violations
        );
    }
}

// --- window removal ---------------------------------------------------------

// Removing a client unlinks it from every structure that can name a window.
//
// A dangling name is not cosmetic: `presented_maximize` and `pending_focus` are
// read by the projection to decide which window to place, and a stale entry
// points it at a destroyed window. The contract is deliberately a full sweep, so
// this checks the sweep rather than one call site at a time.
proptest! {
    #[test]
    fn removing_a_window_leaves_no_reference_anywhere(
        spec in arb_spec(),
        victim in 0usize..8,
    ) {
        let Built { mut state, wins, .. } = build(&spec);
        let Some(&win) = wins.get(victim) else { return Ok(()) };
        let before = state.check_invariants();
        prop_assert!(before.is_ok(), "the fixture state is already broken: {:?}", before);

        let removed = state.remove_client(win);
        prop_assert_eq!(removed.map(|c| c.window), Some(win), "the client was not returned");
        prop_assert!(!state.clients.contains_key(&win), "the client is still managed");
        for mon in &state.monitors {
            prop_assert!(!mon.focus_stack.contains(&win), "the focus stack still names it");
            for ws in &mon.workspaces {
                prop_assert!(!ws.floats.contains(&win), "a float still names it");
                prop_assert!(ws.presented_maximize != Some(win), "a presented overlay still names it");
                for col in &ws.columns {
                    prop_assert!(!col.windows.contains(&win), "a column still names it");
                }
            }
        }
        prop_assert!(
            !state.pending_focus.is_some_and(|pf| pf.window == win || pf.owner == win),
            "a deferral still names it"
        );
        prop_assert!(
            !state.pending_transients.contains(&win),
            "a pending transient set still names it"
        );
        prop_assert!(!state.x11_input_focus.is_some_and(|w| w == win), "the X focus still names it");
        for child in state.clients.values() {
            prop_assert!(child.transient_parent != Some(win), "a child is still parented to it");
        }
        let after = state.check_invariants();
        prop_assert!(after.is_ok(), "removing a window left a broken state: {:?}", after);
    }
}

// A single-monitor workspace holding `wins` tiled clients, with the logical
// focus, the focus stack and the X focus mirror all on the last one added — the
// state `reconcile_focus` leaves behind once the WM has taken focus.
fn x11_mirror_state(wins: &[WindowId]) -> (State, WindowId) {
    let mut state = single_monitor_state();
    for &win in wins {
        add_plain_client(&mut state, win);
        state.monitors[0].workspaces[0].add_tiled(win, 0.5);
    }
    let focused = *wins.last().expect("the fixture was given windows");
    focus_logically(&mut state, 0, focused);
    state.monitors[0].focus_stack = wins.to_vec();
    state.x11_input_focus = Some(focused);
    let before = state.check_invariants();
    assert!(
        before.is_ok(),
        "the fixture state is already broken: {before:?}"
    );
    (state, focused)
}

// The mirror must not outlive the client it names.
//
// The sweep above covers this for every window of every generated state; this
// pins the one case that matters on its own, because it is the only ordering in
// which the mirror is *supposed* to be cleared: the X focus is the client that
// just died, so nothing else in the state names it any more.
#[test]
fn removing_the_x11_focused_client_clears_the_focus_mirror() {
    let (mut state, focused) = x11_mirror_state(&[0x201, 0x202, 0x203]);

    let removed = state.remove_client(focused);

    assert_eq!(
        removed.map(|c| c.window),
        Some(focused),
        "the client was not returned"
    );
    assert_eq!(
        state.x11_input_focus, None,
        "the mirror still names the removed client"
    );
    let after = state.check_invariants();
    assert!(
        after.is_ok(),
        "removing the focused client left a broken state: {after:?}"
    );
}

// ...and it must survive the removal of some *other* client.
//
// This is the half that rules out "clear the mirror unconditionally": the mirror
// is the WM's record of a window the WM manages, so a client that is still alive
// is still what the X server reports, and a removal elsewhere in the state is not
// a reason to forget it.
#[test]
fn removing_another_client_leaves_the_focus_mirror_alone() {
    let (mut state, focused) = x11_mirror_state(&[0x201, 0x202, 0x203]);
    let other = 0x201;
    assert_ne!(other, focused, "the fixture removed the focused client");

    let removed = state.remove_client(other);

    assert_eq!(
        removed.map(|c| c.window),
        Some(other),
        "the client was not returned"
    );
    assert_eq!(
        state.x11_input_focus,
        Some(focused),
        "an unrelated removal took the X focus"
    );
    let after = state.check_invariants();
    assert!(
        after.is_ok(),
        "removing an unfocused client left a broken state: {after:?}"
    );
}

// --- the focus pointer inside a workspace -----------------------------------

// A script of workspace mutations keeps the model valid and the focus pointers
// honest: whatever shape the tree ends up in, the window the core reports as
// focused is the one the focus column and row actually name, and the model
// checker agrees.
proptest! {
    #[test]
    fn a_workspace_script_keeps_the_focus_pointers_honest(ops in arb_workspace_op()) {
        let mut state = single_monitor_state();
        let mut next = 0x1000u32;
        for op in &ops {
            match *op {
                WorkspaceOp::Add { width } => {
                    let win = next;
                    next += 1;
                    add_plain_client(&mut state, win);
                    state.monitors[0].workspaces[0].add_tiled(win, width);
                    // Documented: the insert lands to the right of the focused
                    // column and the new column takes the focus.
                    let ws = &state.monitors[0].workspaces[0];
                    let ci = ws.focus.column_idx;
                    prop_assert!(ci < ws.columns.len(), "the insert left the focus outside the tree");
                    prop_assert_eq!(ws.columns[ci].windows.as_slice(), &[win], "a fresh column is not a singleton");
                    prop_assert_eq!(ws.index_of_window(win), Some((ci, 0)), "the insert is not where focus points");
                    prop_assert_eq!(ws.focused_win(), Some(win), "the insert did not take the focus");
                }
                WorkspaceOp::Drop { ci, pos, from } => {
                    let Some(win) = tree_window(&state.monitors[0].workspaces[0], from) else { continue };
                    {
                        let ws = &mut state.monitors[0].workspaces[0];
                        ws.remove_window(win);
                        ws.drop_into_column(ci, win, pos);
                    }
                    // The drop is refused outright when the column index no longer
                    // names a column, which the unlink above can cause.
                    if ci < state.monitors[0].workspaces[0].columns.len() {
                        // Documented: the row is clamped to the column's last
                        // row, and the dropped window takes the focus.
                        let ws = &state.monitors[0].workspaces[0];
                        let row = pos.min(ws.columns[ci].windows.len() - 1);
                        prop_assert_eq!(ws.columns[ci].windows[row], win, "the drop landed elsewhere");
                        prop_assert_eq!(ws.index_of_window(win), Some((ci, row)), "the row pointer disagrees");
                        prop_assert_eq!(ws.focus.column_idx, ci, "the column pointer disagrees");
                        prop_assert_eq!(ws.focused_win(), Some(win), "the drop did not take the focus");
                    }
                }
                WorkspaceOp::Remove { idx } => {
                    let ws = &state.monitors[0].workspaces[0];
                    let Some(win) = tree_window(ws, idx) else { continue };
                    let focused_before = ws.focused_win();
                    state.monitors[0].workspaces[0].remove_window(win);
                    let ws = &state.monitors[0].workspaces[0];
                    // Documented: a removal before the focused row shifts the row
                    // pointer with it, so the focused *window* is unchanged
                    // unless it was the one removed.
                    if focused_before != Some(win) {
                        prop_assert_eq!(ws.focused_win(), focused_before, "a removal moved the focus to another window");
                    }
                }
                WorkspaceOp::Clean => state.monitors[0].workspaces[0].cleanup_empty_columns(),
                WorkspaceOp::Rebalance => state.monitors[0].workspaces[0].rebalance_weights(),
            }
            let ws = &state.monitors[0].workspaces[0];
            if let Some(w) = ws.focused_win() {
                let ci = ws.focus.column_idx;
                prop_assert!(ci < ws.columns.len(), "the focus pointer left the tree");
                prop_assert_eq!(ws.columns[ci].focused_win(), Some(w), "the column pointer disagrees with the tree");
                prop_assert_eq!(
                    ws.index_of_window(w),
                    Some((ci, ws.columns[ci].focused)),
                    "the row pointer disagrees with the tree"
                );
            }
            for (ci, col) in ws.columns.iter().enumerate() {
                prop_assert!(!col.windows.is_empty(), "column {} is empty", ci);
                prop_assert!(col.focused < col.windows.len(), "the row pointer left column {}", ci);
            }
            let checked = state.check_invariants();
            prop_assert!(checked.is_ok(), "the workspace script broke the model: {:?}", checked);
        }
    }
}

// Dropping empty columns keeps the focus on the same *window*, not merely on a
// legal index.
//
// A bare clamp would leave the focus on whichever column happened to occupy the
// clamped index, mis-centering the camera and handing the user's keystrokes to a
// neighbour — the exact failure `cleanup_empty_columns` documents.
proptest! {
    #[test]
    fn dropping_empty_columns_keeps_the_focus_on_the_same_window(
        windows in 1usize..=6,
        gaps in proptest::collection::vec(0usize..=6, 1..6),
    ) {
        let target = (windows - 1) / 2;
        let mut ws = Workspace::new(0);
        for i in 0..windows {
            ws.add_tiled(0x100 + i as u32, 0.5);
        }
        // Splice empty columns in at the requested offsets so the cleanup has
        // something to drop before, at, and after the focus.
        for gap in &gaps {
            let at = (*gap).min(ws.columns.len());
            ws.columns.insert(at, Column::new(0.5));
        }
        // Point the focus at the column that holds window number `target`,
        // wherever the splices pushed it.
        let (ci, _) = ws
            .index_of_window(0x100 + target as u32)
            .expect("a window that was just added");
        ws.focus.column_idx = ci;
        ws.columns[ci].focused = 0;
        let before = ws.focused_win();
        prop_assert!(before.is_some(), "the fixture has no focused window");

        ws.cleanup_empty_columns();

        prop_assert_eq!(ws.focused_win(), before, "the focus moved to another column");
        prop_assert!(ws.columns.iter().all(|c| !c.windows.is_empty()), "an empty column survived");
        if let Some(w) = before {
            let (ci, ri) = ws.index_of_window(w).expect("the focused window is no longer in the tree");
            prop_assert_eq!(ci, ws.focus.column_idx, "the column pointer disagrees with the tree");
            prop_assert_eq!(ri, ws.columns[ci].focused, "the row pointer disagrees with the tree");
        }
    }
}

// --- column weights ---------------------------------------------------------

// A tiling request can name any width at all and must not be able to steer a
// column outside the documented `[0.1, 1.0]` band.
//
// `column_width` arrives from the user config and the layout clamps tiled geometry
// against it: a NaN width produces NaN geometry, and a width above 1.0 would
// claim more screen than the workarea has.
proptest! {
    #[test]
    fn a_tiling_request_always_yields_an_in_bounds_column(
        width in prop_oneof![any::<f32>(), 0.0f32..=1.5, Just(0.0), Just(-1.0), Just(f32::NAN), Just(f32::INFINITY)],
        more in proptest::collection::vec(prop_oneof![any::<f32>(), 0.0f32..=1.5], 0..6),
    ) {
        let mut ws = Workspace::new(0);
        ws.add_tiled(0x1, width);
        for w in more {
            ws.add_tiled(0x2 + (w.to_bits() % 1000), w);
        }
        for (ci, col) in ws.columns.iter().enumerate() {
            prop_assert!(
                col.weight.is_finite() && (0.1..=1.0).contains(&col.weight),
                "column {} has weight {}",
                ci,
                col.weight
            );
        }
    }
}

// `rebalance_weights` is a repair, not a redistribution: a column that already
// has a usable width keeps it exactly and only a broken one is replaced.
//
// The whole point of the true-scroll model is that adding, growing or removing a
// column never resizes its neighbours, so a repair that nudged healthy widths
// would silently reflow the ribbon.
proptest! {
    #[test]
    fn rebalancing_repairs_only_the_broken_weights(
        weights in proptest::collection::vec(
            prop_oneof![0.0f32..=1.0, Just(0.0), Just(-0.5), Just(f32::NAN), Just(f32::INFINITY)],
            1..8,
        ),
    ) {
        let mut ws = Workspace::new(0);
        for (i, w) in weights.iter().enumerate() {
            ws.add_tiled(0x200 + i as u32, 1.0);
            ws.columns.last_mut().expect("just added").weight = *w;
        }
        let before: Vec<f32> = ws.columns.iter().map(|c| c.weight).collect();

        ws.rebalance_weights();

        for (i, col) in ws.columns.iter().enumerate() {
            let healthy = before[i].is_finite() && before[i] > 0.0;
            prop_assert!(col.weight.is_finite() && col.weight > 0.0, "column {} is still broken at {}", i, col.weight);
            if healthy {
                prop_assert_eq!(col.weight, before[i], "a healthy weight was redistributed");
            }
        }
    }
}

// Splitting a column hands half its width to each half, so a chain of splits
// drives a column's weight down without bound. Every weight must still be inside
// the `[0.05, 1.0]` band invariant F declares, at every step — `GrowColumn` and
// `add_tiled` both clamp into that band, and `Engine::execute` runs the checker
// after every `MoveWindow`, so a single step outside the band aborts a debug
// build.
proptest! {
    #[test]
    fn splitting_a_column_keeps_every_weight_inside_the_documented_band(
        width in prop_oneof![0.05f32..=1.0, Just(0.1), Just(0.05)],
        windows in 1usize..=8,
        splits in 0usize..=8,
    ) {
        let mut state = single_monitor_state();
        let wins: Vec<WindowId> = (0..windows).map(|i| 0x300 + i as u32).collect();
        for w in &wins {
            add_plain_client(&mut state, *w);
            state.monitors[0].workspaces[0].add_tiled(*w, width);
        }
        {
            // Merge every window into the first column — what a drag into a column
            // followed by the removal of the emptied ones leaves behind — and give
            // it the requested width, which is a value the tiling path itself
            // writes (`add_tiled` clamps a request into [0.1, 1.0], `GrowColumn`
            // into [0.05, 1.0]).
            let ws = &mut state.monitors[0].workspaces[0];
            for w in wins.iter().skip(1) {
                ws.remove_window(*w);
            }
            for w in wins.iter().skip(1) {
                let pos = ws.columns[0].windows.len();
                ws.drop_into_column(0, *w, pos);
            }
            ws.columns[0].weight = width;
        }
        for _ in 0..splits {
            {
                // Point the focus at the leading window of the column that still
                // holds more than one, exactly as a focus command would.
                let ws = &mut state.monitors[0].workspaces[0];
                if ws.columns[0].windows.len() < 2 {
                    break;
                }
                ws.focus.column_idx = 0;
                ws.columns[0].focused = 0;
                let win = ws.columns[0].windows[0];
                state.monitors[0].focused = Some(win);
            }
            prop_assert!(state.apply_move_dir(Dir::Right), "the split did not happen");
            let ws = &state.monitors[0].workspaces[0];
            for (ci, col) in ws.columns.iter().enumerate() {
                prop_assert!(
                    (0.05..=1.0).contains(&col.weight),
                    "column {} weighs {} after splitting a column that started at {}",
                    ci,
                    col.weight,
                    width
                );
            }
            let checked = state.check_invariants();
            prop_assert!(checked.is_ok(), "splitting broke the model: {:?}", checked);
        }
    }
}

// --- movement ---------------------------------------------------------------

// Moving the focused window never changes which window is focused and never
// invents or destroys a window; a move the WM refuses changes nothing at all.
//
// `MoveWindow` is a pure rearrangement of the ribbon: the user pressed a movement
// key, so the window under their hands must stay under their hands while
// everything else reflows around it. And a refused move — a float, an empty
// workspace, a boundary with nothing to swap — must not leave a partial
// rearrangement behind.
proptest! {
    #[test]
    fn a_move_never_changes_the_focused_window_or_the_window_set(
        spec in arb_spec(),
        dir in arb_dir(),
    ) {
        let Built { mut state, .. } = build(&spec);
        let before = state.check_invariants();
        prop_assert!(before.is_ok(), "the fixture state is already broken: {:?}", before);
        // Only a workspace with a tiled focus can move; floats and empty
        // workspaces are documented no-ops.
        let mi = state.sel_mon;
        let ws_i = state.monitors[mi].active_ws;
        // `apply_move_dir` rearranges the window the *monitor's* logical focus
        // names, and leaves that window focused afterwards.
        let Some(focused) = state.monitors[mi].focused else { return Ok(()) };
        if state.monitors[mi].workspaces[ws_i].index_of_window(focused).is_none() {
            return Ok(());
        }
        let before_wins = window_set(&state, mi, ws_i);
        let shape_before = workspace_shape(&state, mi, ws_i);

        let moved = state.apply_move_dir(dir);

        let ws = &state.monitors[mi].workspaces[ws_i];
        prop_assert_eq!(window_set(&state, mi, ws_i), before_wins, "the move changed the window set");
        if moved {
            prop_assert_eq!(ws.focused_win(), Some(focused), "the move changed the focused window");
            prop_assert!(
                state.monitors[mi].workspaces[ws_i].index_of_window(focused).is_some(),
                "the move lost the focused window"
            );
        } else {
            prop_assert_eq!(
                workspace_shape(&state, mi, ws_i),
                shape_before,
                "a refused move still rearranged the tree"
            );
        }
    }
}

// A focus pointer left stale by a shrink or a session restore must make a move a
// no-op, not a panic.
//
// `apply_move_dir` documents exactly this bail-out: the restore path can leave
// `column_idx` past the end of the column list, and indexing it would take the
// whole window manager down on a state it is about to repair.
proptest! {
    #[test]
    fn a_stale_focus_pointer_turns_every_move_into_a_no_op(stale in 1usize..=64) {
        let mut state = single_monitor_state();
        // One column holding three windows, so a stale column index is the only
        // thing standing between the move and the `columns[ci]` index.
        for i in 0..3u32 {
            let win = 0x400 + i;
            add_plain_client(&mut state, win);
            state.monitors[0].workspaces[0].drop_into_column(0, win, i as usize);
        }
        state.monitors[0].focused = Some(0x401);
        state.monitors[0].workspaces[0].focus.column_idx = stale;
        let shape = workspace_shape(&state, 0, 0);

        for dir in [Dir::Left, Dir::Right, Dir::Up, Dir::Down, Dir::Next, Dir::Prev] {
            prop_assert!(!state.apply_move_dir(dir), "{dir:?} acted on a stale focus pointer");
        }
        prop_assert_eq!(workspace_shape(&state, 0, 0), shape, "a refused move still rearranged the tree");
    }
}

// --- flag words -------------------------------------------------------------

// Each flag predicate reads exactly the bit it documents, and nothing else.
//
// The layout, the projection and the window rules all branch on these
// predicates, so one that answered from a neighbouring bit would make a
// single-axis maximize read as a full one, or an ordinary tile read as a float.
// Bits 3, 4 and 9 and up are documented as reserved, so a random word
// exercises them too.
proptest! {
    #[test]
    fn every_flag_predicate_reads_only_its_documented_bit(
        word in any::<u16>(),
        policy in arb_fullscreen_policy(),
    ) {
        let mut c = Client::new(1, 0, 0);
        c.flags = flags_of(word);
        c.fullscreen_policy = policy;

        let has = |bit: u16| word & bit != 0;
        prop_assert_eq!(c.is_float(), has(WinFlags::FLOAT), "is_float read the wrong bit");
        prop_assert_eq!(c.is_fullscreen(), has(WinFlags::FULLSCREEN), "is_fullscreen read the wrong bit");
        prop_assert_eq!(c.is_sticky(), has(WinFlags::STICKY), "is_sticky read the wrong bit");
        prop_assert_eq!(c.is_maximized_v(), has(WinFlags::MAXIMIZED_V), "is_maximized_v read the wrong bit");
        prop_assert_eq!(c.is_maximized_h(), has(WinFlags::MAXIMIZED_H), "is_maximized_h read the wrong bit");
        prop_assert_eq!(c.flags.has(WinFlags::URGENT), has(WinFlags::URGENT), "URGENT is not readable");
        prop_assert_eq!(c.flags.has(WinFlags::FS_WAS_FLOAT), has(WinFlags::FS_WAS_FLOAT), "FS_WAS_FLOAT is not readable");
        // The client's input model (`WM_HINTS.input`) decides focus eligibility
        // and is deliberately NOT one of these bits: a client may rewrite the
        // property at any time and ICCCM 4.1.2 requires the WM to retain no
        // memory of the old value, which a set-only flag word cannot express.
        // The bit-layout test below is where that is pinned down.
        // Documented: only both axes together mean "maximized", because EWMH
        // treats them as independent states and clients request one of them.
        prop_assert_eq!(
            c.is_maximized(),
            has(WinFlags::MAXIMIZED_V) && has(WinFlags::MAXIMIZED_H),
            "is_maximized is not the conjunction of the two axes"
        );
        prop_assert_eq!(
            c.is_fullscreen_overlay(),
            has(WinFlags::FULLSCREEN) && policy == FullscreenPolicy::True,
            "is_fullscreen_overlay ignored the policy"
        );
        prop_assert_eq!(c.denies_fullscreen(), policy == FullscreenPolicy::Deny, "denies_fullscreen ignored the policy");
        prop_assert_eq!(WinFlags::MAXIMIZED, WinFlags::MAXIMIZED_V | WinFlags::MAXIMIZED_H);
    }
}

// The flag and hint constants are the only place Maverick agrees with the
// *outside* about what a number means. Every other property in this file reads
// them symbolically — `WinFlags::FLOAT` wherever a float is expected — so they
// stay self-consistent under any redefinition and would all pass if `FLOAT`
// were silently moved to bit 4. Their wire values are protocol commitments:
// `WinFlags` mirrors `_NET_WM_STATE`, and `SizeHints` mirrors ICCCM 4.1.2.3's
// `XSizeHints.flags`.
//
// So this table is stated as literals, not in terms of the constants. It also
// pins the two structural facts the rest of the WM relies on and that no other
// test asserts: the used bits are exactly 0..=2 and 5..=8, and no two constants share a
// bit. `MAXIMIZED_H` is deliberately *not* adjacent to `MAXIMIZED_V` — the two
// EWMH axes are independent states, and nothing else in the tree would notice
// if the pair collapsed onto neighbouring bits and a single-axis maximize began
// reading as a full one.
#[test]
fn the_ewmh_and_icccm_bit_layout_is_the_one_the_protocol_defines() {
    assert_eq!(WinFlags::FLOAT, 1 << 0, "_NET_WM_STATE_FLOAT");
    assert_eq!(WinFlags::FULLSCREEN, 1 << 1, "_NET_WM_STATE_FULLSCREEN");
    assert_eq!(WinFlags::URGENT, 1 << 2, "_NET_WM_STATE_DEMANDS_ATTENTION");
    assert_eq!(
        WinFlags::MAXIMIZED_V,
        1 << 5,
        "_NET_WM_STATE_MAXIMIZED_VERT"
    );
    assert_eq!(WinFlags::STICKY, 1 << 6);
    assert_eq!(WinFlags::FS_WAS_FLOAT, 1 << 7);
    assert_eq!(
        WinFlags::MAXIMIZED_H,
        1 << 8,
        "_NET_WM_STATE_MAXIMIZED_HORZ"
    );
    assert_eq!(
        WinFlags::MAXIMIZED,
        WinFlags::MAXIMIZED_V | WinFlags::MAXIMIZED_H
    );

    assert_eq!(SizeHints::U_S_POSITION, 1 << 0, "XUSPosition");
    assert_eq!(SizeHints::U_S_SIZE, 1 << 1, "XUSSize");
    assert_eq!(SizeHints::P_POSITION, 1 << 2, "XPosition");
    assert_eq!(SizeHints::P_SIZE, 1 << 3, "XPSize");
    assert_eq!(SizeHints::P_MIN_SIZE, 1 << 4, "XPMinSize");
    assert_eq!(SizeHints::P_MAX_SIZE, 1 << 5, "XPMaxSize");
    assert_eq!(SizeHints::P_RESIZE_INC, 1 << 6, "XPResizeInc");
    assert_eq!(SizeHints::P_ASPECT, 1 << 7, "XPAspect");
    assert_eq!(SizeHints::P_BASE_SIZE, 1 << 8, "XPBaseSize");
    assert_eq!(SizeHints::P_WIN_GRAVITY, 1 << 9, "XPWinGravity");

    // Every bit is distinct, and the used range is exactly 0..=2 and 5..=8.
    let used = [
        WinFlags::FLOAT,
        WinFlags::FULLSCREEN,
        WinFlags::URGENT,
        WinFlags::MAXIMIZED_V,
        WinFlags::STICKY,
        WinFlags::FS_WAS_FLOAT,
        WinFlags::MAXIMIZED_H,
    ];
    let mut all = 0u16;
    for (i, f) in used.iter().enumerate() {
        assert_eq!(
            f.count_ones(),
            1,
            "flag {i} ({f:#06x}) must occupy exactly one bit"
        );
        assert_eq!(
            all & f,
            0,
            "flag {i} ({f:#06x}) shares a bit with an earlier one"
        );
        all |= f;
    }
    // 0x01E7, not the 0x01EF this used to assert: bit 3 held the `WM_HINTS`
    // input model, which is the client's own declaration rather than a
    // presentation policy the WM decides, and a set-only flag bit could not
    // track it — ICCCM 4.1.2 requires the WM to honour a rewrite of the
    // property with no memory of the old value, which means the bit had to be
    // clearable, and every other bit here is deliberately one-way. So the input
    // model lives in `Client::wants_input` alone and bit 3 is reserved, which is
    // exactly what the layout's own rule prescribes for a vacated bit: leave the
    // gap so `MAXIMIZED_V` and the rest keep the values their protocols name.
    assert_eq!(
        all, 0x01E7,
        "bits 0..=2 and 5..=8 are in use; 3, 4 and 9..=15 are reserved"
    );
}

// The input model has exactly one home. This is the state a focus decision is
// made from, and it is reached by assignment on every `WM_HINTS` rewrite, so a
// window that declares `input = False` and then re-declares `input = True`
// must land in precisely the state a window that had declared `True` all along
// is in — the invariant the removed input bit could not hold, because it was
// only ever set: the flip left the bit asserting "no input" beside a field
// asserting "wants input", and every later focus request was refused.
#[test]
fn the_input_model_is_a_field_no_flag_bit_can_shadow() {
    const USED: [u16; 7] = [
        WinFlags::FLOAT,
        WinFlags::FULLSCREEN,
        WinFlags::URGENT,
        WinFlags::MAXIMIZED_V,
        WinFlags::STICKY,
        WinFlags::FS_WAS_FLOAT,
        WinFlags::MAXIMIZED_H,
    ];
    let mut c = Client::new(7, 0, 0);
    assert!(c.wants_input, "a client that declared nothing wants input");
    c.wants_input = false;
    for f in USED {
        assert!(
            !c.flags.has(f),
            "declaring input=false set flag {f:#06x}: the input model is not a presentation policy"
        );
    }
    c.wants_input = true;
    assert!(
        c.wants_input,
        "re-declaring input=true did not restore eligibility"
    );
    for f in USED {
        assert!(
            !c.flags.has(f),
            "re-declaring input=true set flag {f:#06x}: the input model is not a presentation policy"
        );
    }
}

// A column added and then removed again must leave the workspace exactly as it
// found it. This is the round trip every float↔fullscreen transition makes —
// `apply_fullscreen_topology` promotes a float into a column and demotes it
// back — and it is the only way a single user-visible toggle pair can leave a
// permanent mark on the workspace.
//
// The pointer that drifts is `focus.column_idx`, and it is load-bearing twice
// over: `ideal_scroll` reads it to decide where the camera rests, and
// `best_focus` reads it to decide which window the keyboard goes to. A drift of
// one is not cosmetic — it centres the camera on a neighbour and sends focus to
// a window the user never selected, and it accumulates every time the toggle is
// used.
#[test]
fn adding_a_column_and_removing_it_again_restores_the_workspace() {
    for n in 1..=5usize {
        for active in 0..n {
            let mut ws = Workspace::new(0);
            for i in 0..n {
                ws.add_tiled(1000 + i as u32, 0.5);
            }
            // Put the focus where the case is about, rather than wherever
            // `add_tiled` happened to leave it (which is the last column).
            ws.focus.column_idx = active;
            // Add one more column, exactly as a promoted float is added, then
            // take it back out. `add_tiled` places the new column after the
            // focus and moves the focus onto it, so the pair has to be
            // symmetric.
            ws.add_tiled(9999, 0.5);
            ws.remove_window(9999);

            assert_eq!(
                ws.focus.column_idx, active,
                "{n} columns, focus {active}: the add/remove round trip left the \
                 focus on column {} — the camera centres the wrong column and \
                 best_focus disagrees with mon.focused",
                ws.focus.column_idx
            );
            assert_eq!(
                ws.columns.len(),
                n,
                "{n} columns, focus {active}: the round trip changed the column count"
            );
            let want: Vec<WindowId> = (0..n).map(|i| 1000 + i as u32).collect();
            let got: Vec<WindowId> = ws.columns.iter().flat_map(|c| c.windows.clone()).collect();
            assert_eq!(got, want, "{n} columns, focus {active}: the tree changed");
        }
    }
}

// `set`, `clear` and `toggle` are a set algebra over one word, which is what the
// WM's transition code assumes when it composes several rules at once.
proptest! {
    #[test]
    fn the_flag_mutators_form_a_set_algebra(word in any::<u16>(), a in any::<u16>(), b in any::<u16>()) {
        let mut set_a = flags_of(word);
        set_a.set(a);
        prop_assert_eq!(word_of(&set_a), word | a, "set did not add its bits");

        let mut clear_a = flags_of(word);
        clear_a.clear(a);
        prop_assert_eq!(word_of(&clear_a), word & !a, "clear did not remove its bits");

        let mut toggled = flags_of(word);
        toggled.toggle(a);
        prop_assert_eq!(word_of(&toggled), word ^ a, "toggle did not flip its bits");

        let mut set_then_clear = flags_of(word);
        set_then_clear.set(a);
        set_then_clear.clear(a);
        prop_assert_eq!(
            word_of(&set_then_clear),
            (word | a) & !a,
            "clearing a bit that was just set changed the word"
        );

        let mut twice = flags_of(word);
        twice.set(a);
        twice.set(a);
        prop_assert_eq!(word_of(&twice), word | a, "setting a bit twice is not idempotent");

        let mut round_trip = flags_of(word);
        round_trip.toggle(a);
        round_trip.toggle(a);
        prop_assert_eq!(word_of(&round_trip), word, "toggling a bit twice is not the identity");

        // Setting and clearing commute when the two masks do not overlap.
        if a & b == 0 {
            let mut set_then_clear = flags_of(word);
            set_then_clear.set(a);
            set_then_clear.clear(b);
            let mut clear_then_set = flags_of(word);
            clear_then_set.clear(b);
            clear_then_set.set(a);
            prop_assert_eq!(word_of(&set_then_clear), word_of(&clear_then_set), "setting and clearing disjoint bits do not commute");
        }

        // A mask reports itself set exactly when at least one of its bits is, so
        // the axis predicates compose: knowing a single axis implies knowing the
        // maximize mask that contains it.
        let (bit_a, bit_b) = (1u16 << (a % 16), 1u16 << (b % 16));
        prop_assert_eq!(
            flags_of(word).has(bit_a | bit_b),
            flags_of(word).has(bit_a) || flags_of(word).has(bit_b),
            "a multi-bit mask is not the union of its bits"
        );
    }
}

// --- size hints -------------------------------------------------------------

// A client claims its own position only when ICCCM 4.1.2.3 says so: one of the
// two position bits, and only when any hint was set at all.
//
// Getting this wrong is visible both ways: honouring a stale position hint
// re-centres the window on map, a visible teleport, and ignoring a real one
// drops the window where the app asked for.
proptest! {
    #[test]
    fn a_position_claim_needs_both_the_protocol_bit_and_a_valid_hint_word(
        flags in any::<u32>(),
        valid in any::<bool>(),
    ) {
        let hints = SizeHints {
            flags,
            valid,
            ..SizeHints::default()
        };
        let asked = flags & POSITION_BITS != 0;

        prop_assert_eq!(hints.claims_position(), valid && asked, "the position claim is not the documented conjunction");
        if hints.claims_position() {
            prop_assert!(valid, "an invalid hint word claimed a position");
            prop_assert!(asked, "a word without a position bit claimed a position");
        }
        // Only the two position bits matter: every other bit of the word is
        // irrelevant to the claim.
        for noise in [SizeHints::U_S_SIZE, SizeHints::P_SIZE, SizeHints::P_MIN_SIZE, 1 << 20, 1 << 31] {
            let mut noisy = hints;
            noisy.flags ^= noise;
            prop_assert_eq!(
                noisy.claims_position(),
                hints.claims_position(),
                "unrelated bit {} changed the position claim",
                noise
            );
        }
    }
}

// --- helpers ----------------------------------------------------------------

/// The fullscreen policy a window rule may install.
fn arb_fullscreen_policy() -> impl Strategy<Value = FullscreenPolicy> {
    prop_oneof![
        Just(FullscreenPolicy::Normal),
        Just(FullscreenPolicy::Deny),
        Just(FullscreenPolicy::True)
    ]
}

/// A `WinFlags` holding exactly `word`, built through the public mutators.
fn flags_of(word: u16) -> WinFlags {
    let mut f = WinFlags::default();
    f.clear(WinFlags::MAXIMIZED);
    f.set(word);
    f
}

/// Read a `WinFlags` word back out. The type is deliberately opaque, so probing
/// every bit is the only way to compare two of them for equality.
fn word_of(f: &WinFlags) -> u16 {
    (0..16).fold(0u16, |acc, b| acc | if f.has(1 << b) { 1 << b } else { 0 })
}

/// A one-monitor, one-workspace state, the smallest host for the tree fixtures.
fn single_monitor_state() -> State {
    let mut state = State::new();
    state
        .monitors
        .push(Monitor::new(Rect::new(0, 0, 1920, 1080), 1));
    state
}

/// Register an ordinary, unmaximized client. `Client::new` starts a client out
/// maximized, which would make it a presentation overlay and change what the
/// fixtures are testing.
fn add_plain_client(state: &mut State, win: WindowId) {
    let mut c = Client::new(win, 0, 0);
    c.flags.clear(WinFlags::MAXIMIZED);
    state.add_client(c);
}

/// The `idx`-th window in the tiled tree, in column-major order.
fn tree_window(ws: &Workspace, idx: usize) -> Option<WindowId> {
    ws.columns
        .iter()
        .flat_map(|c| c.windows.iter().copied())
        .nth(idx)
}

/// The sorted window set of a workspace, for "nothing was gained or lost".
fn window_set(st: &State, mi: usize, ws_i: usize) -> Vec<WindowId> {
    let mut v: Vec<WindowId> = st.monitors[mi].workspaces[ws_i]
        .columns
        .iter()
        .flat_map(|c| c.windows.iter().copied())
        .collect();
    v.sort_unstable();
    v
}

/// A comparable snapshot of a workspace's placement, focus and camera, so
/// "nothing changed" is a checkable claim rather than an `==` on a type with no
/// `PartialEq`.
#[derive(Debug, PartialEq)]
struct WsShape {
    columns: Vec<(Vec<WindowId>, usize, f32)>,
    floats: Vec<WindowId>,
    focus: usize,
    camera: f32,
}

fn workspace_shape(st: &State, mi: usize, ws_i: usize) -> WsShape {
    let ws = &st.monitors[mi].workspaces[ws_i];
    WsShape {
        columns: ws
            .columns
            .iter()
            .map(|c| (c.windows.clone(), c.focused, c.weight))
            .collect(),
        floats: ws.floats.clone(),
        focus: ws.focus.column_idx,
        camera: ws.camera.position,
    }
}

/// A mutation of a workspace's tree, as the drag path and the tiling path perform
/// them.
#[derive(Debug, Clone)]
enum WorkspaceOp {
    Add { width: f32 },
    Drop { ci: usize, pos: usize, from: usize },
    Remove { idx: usize },
    Clean,
    Rebalance,
}

fn arb_workspace_op() -> impl Strategy<Value = Vec<WorkspaceOp>> {
    proptest::collection::vec(
        prop_oneof![
            (prop_oneof![any::<f32>(), 0.0f32..=1.0]).prop_map(|width| WorkspaceOp::Add { width }),
            (0usize..=4, 0usize..=4, 0usize..=3).prop_map(|(ci, pos, from)| WorkspaceOp::Drop {
                ci,
                pos,
                from
            }),
            (0usize..=3).prop_map(|idx| WorkspaceOp::Remove { idx }),
            Just(WorkspaceOp::Clean),
            Just(WorkspaceOp::Rebalance),
        ],
        1..12,
    )
}

// --- invariant injections ---------------------------------------------------

/// One way to corrupt a single field of an otherwise valid state.
#[derive(Debug, Clone, Copy)]
enum Violation {
    ClientMonitorOutOfRange,
    ClientWorkspaceOutOfRange,
    ColumnNamesAnUnknownClient,
    ClientReferencedTwice,
    ClientStoredInTheWrongWorkspace,
    FocusColumnOutOfRange,
    ColumnRowOutOfRange,
    CameraCarriesNaN,
    FocusStackNamesAnUnknownClient,
    FocusStackHasADuplicate,
    WeightBelowTheFloor,
    WeightAboveTheCeiling,
    WeightIsNotFinite,
    ActiveWorkspaceOutOfRange,
    PresentedMaximizeIsNotMaximized,
    InputFocusNamesAnUnknownClient,
    DeferredFocusNamesAnUnknownClient,
    DeferredFocusOwnerIsNotPresented,
}

impl Violation {
    /// The injection space, one strategy per corruption.
    fn strategy() -> impl Strategy<Value = Self> {
        prop_oneof![
            Just(Self::ClientMonitorOutOfRange),
            Just(Self::ClientWorkspaceOutOfRange),
            Just(Self::ColumnNamesAnUnknownClient),
            Just(Self::ClientReferencedTwice),
            Just(Self::ClientStoredInTheWrongWorkspace),
            Just(Self::FocusColumnOutOfRange),
            Just(Self::ColumnRowOutOfRange),
            Just(Self::CameraCarriesNaN),
            Just(Self::FocusStackNamesAnUnknownClient),
            Just(Self::FocusStackHasADuplicate),
            Just(Self::WeightBelowTheFloor),
            Just(Self::WeightAboveTheCeiling),
            Just(Self::WeightIsNotFinite),
            Just(Self::ActiveWorkspaceOutOfRange),
            Just(Self::PresentedMaximizeIsNotMaximized),
            Just(Self::InputFocusNamesAnUnknownClient),
            Just(Self::DeferredFocusNamesAnUnknownClient),
            Just(Self::DeferredFocusOwnerIsNotPresented)
        ]
    }

    /// Corrupt exactly one thing and report the wording of the clause that owns
    /// it, so the property can insist that the *right* check fired.
    fn apply(self, st: &mut State) -> &'static str {
        match self {
            Self::ClientMonitorOutOfRange => {
                let win = any_client(st);
                let n = st.monitors.len() + 7;
                st.clients.get_mut(&win).expect("a managed client").monitor = n;
                "out of range"
            }
            Self::ClientWorkspaceOutOfRange => {
                let win = any_client(st);
                let mi = st.clients[&win].monitor as usize;
                let n = st.monitors[mi].workspaces.len() + 3;
                st.clients
                    .get_mut(&win)
                    .expect("a managed client")
                    .workspace = n;
                "out of range"
            }
            Self::ColumnNamesAnUnknownClient => {
                let (mi, ws_i, ci) = ensure_column(st);
                st.monitors[mi].workspaces[ws_i].columns[ci].windows[0] = 0xDEAD_BEEF;
                "not in clients"
            }
            Self::ClientReferencedTwice => {
                let (mi, ws_i, ci) = ensure_column(st);
                let win = st.monitors[mi].workspaces[ws_i].columns[ci].windows[0];
                st.monitors[mi].workspaces[ws_i].columns[ci]
                    .windows
                    .push(win);
                "referenced twice"
            }
            Self::ClientStoredInTheWrongWorkspace => {
                // A window that moved without being re-placed: the tree and the
                // client's own placement record disagree about where it lives.
                let win = any_client(st);
                let mi = st.clients[&win].monitor as usize;
                let fresh = st.monitors[mi].workspaces.len();
                st.monitors[mi]
                    .workspaces
                    .push(Workspace::new(fresh as u32));
                st.monitors[mi].workspaces[fresh].add_tiled(win, 0.5);
                "stored at"
            }
            Self::FocusColumnOutOfRange => {
                let (mi, ws_i, _) = ensure_column(st);
                let ws = &mut st.monitors[mi].workspaces[ws_i];
                ws.focus.column_idx = ws.columns.len();
                "focus.column_idx"
            }
            Self::ColumnRowOutOfRange => {
                let (mi, ws_i, ci) = ensure_column(st);
                let col = &mut st.monitors[mi].workspaces[ws_i].columns[ci];
                col.focused = col.windows.len();
                "focused"
            }
            Self::CameraCarriesNaN => {
                st.monitors[0].workspaces[0].camera.position = f32::NAN;
                "camera has NaN"
            }
            Self::FocusStackNamesAnUnknownClient => {
                st.monitors[0].focus_stack.push(0xBAD0_BEEF);
                "focus_stack references unknown window"
            }
            Self::FocusStackHasADuplicate => {
                let first = st.monitors[0].focus_stack.first().copied();
                let win = first.unwrap_or_else(|| any_client(st));
                let stack = &mut st.monitors[0].focus_stack;
                stack.push(win);
                stack.push(win);
                "focus_stack has duplicate entries"
            }
            Self::WeightBelowTheFloor => {
                let (mi, ws_i, ci) = ensure_column(st);
                st.monitors[mi].workspaces[ws_i].columns[ci].weight = 0.0;
                "fuera de"
            }
            Self::WeightAboveTheCeiling => {
                let (mi, ws_i, ci) = ensure_column(st);
                st.monitors[mi].workspaces[ws_i].columns[ci].weight = 1.5;
                "fuera de"
            }
            Self::WeightIsNotFinite => {
                let (mi, ws_i, ci) = ensure_column(st);
                st.monitors[mi].workspaces[ws_i].columns[ci].weight = f32::NAN;
                "no finito"
            }
            Self::ActiveWorkspaceOutOfRange => {
                let mon = &mut st.monitors[0];
                mon.active_ws = mon.workspaces.len() + 2;
                "active_ws"
            }
            Self::PresentedMaximizeIsNotMaximized => {
                // A maximized overlay on a workspace that is not the active one
                // is explicitly allowed, so this only breaks if the client stops
                // being maximized at all.
                let win = maximize_presented_client(st);
                let mi = st.clients[&win].monitor as usize;
                let ws_i = st.clients[&win].workspace as usize;
                let c = st.clients.get_mut(&win).expect("a managed client");
                c.flags.clear(WinFlags::MAXIMIZED);
                c.flags.clear(WinFlags::MAXIMIZED_V);
                c.flags.clear(WinFlags::MAXIMIZED_H);
                st.monitors[mi].workspaces[ws_i].presented_maximize = Some(win);
                "is not maximized"
            }
            Self::InputFocusNamesAnUnknownClient => {
                st.x11_input_focus = Some(0xFEED_FACE);
                "x11_input_focus"
            }
            Self::DeferredFocusNamesAnUnknownClient => {
                let owner = any_client(st);
                st.pending_focus = Some(PendingFocus {
                    window: 0xBAD0_0BAD,
                    owner,
                    monitor: st.clients[&owner].monitor,
                    workspace: st.clients[&owner].workspace,
                });
                "pending_focus window"
            }
            Self::DeferredFocusOwnerIsNotPresented => {
                // A live client that is neither a true-fullscreen overlay nor a
                // focused maximize, so nothing presents it as an overlay.
                let win = 0xDEFE_0001;
                add_plain_client(st, win);
                st.monitors[0].workspaces[0].add_tiled(win, 0.5);
                st.pending_focus = Some(PendingFocus {
                    window: win,
                    owner: win,
                    monitor: 0,
                    workspace: 0,
                });
                "is not a presented overlay"
            }
        }
    }
}

fn arb_violation() -> impl Strategy<Value = Violation> {
    Violation::strategy()
}

/// The lowest managed window id, so fixture choices are reproducible.
fn any_client(st: &mut State) -> WindowId {
    let mut keys: Vec<WindowId> = st.clients.keys().copied().collect();
    if keys.is_empty() {
        add_plain_client(st, 0xC0FF_EE00);
        return 0xC0FF_EE00;
    }
    keys.sort_unstable();
    keys[0]
}

/// A client that *is* the presented maximize of its monitor's active workspace,
/// so a follow-up corruption of the overlay bookkeeping has something real to
/// contradict.
fn maximize_presented_client(st: &mut State) -> WindowId {
    for mon in &st.monitors {
        if let Some(win) = mon.ws().presented_maximize {
            return win;
        }
    }
    let win = 0xDEAD_0001;
    add_plain_client(st, win);
    let c = st.clients.get_mut(&win).expect("just added");
    c.flags.set(WinFlags::MAXIMIZED);
    st.monitors[0].focused = Some(win);
    st.sync_presented_maximize(0);
    win
}

/// Locate a workspace that has at least one column, creating the first one if the
/// fixture has none, so an injection always has something to corrupt.
fn ensure_column(st: &mut State) -> (usize, usize, usize) {
    for (mi, mon) in st.monitors.iter().enumerate() {
        for (ws_i, ws) in mon.workspaces.iter().enumerate() {
            if !ws.columns.is_empty() {
                return (mi, ws_i, 0);
            }
        }
    }
    let win = 0xC0DE_0001;
    add_plain_client(st, win);
    st.monitors[0].workspaces[0].add_tiled(win, 0.5);
    (0, 0, 0)
}
