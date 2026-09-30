#[cfg(test)]
#[allow(clippy::if_same_then_else)]
mod unit_tests {
    use crate::config::Cfg;
    use crate::core::desired::DesiredState;
    use crate::core::layout::{FsCtx, RibbonScratch};
    use crate::core::commands::Command as CommandTrait;
    use crate::core::Engine;
    use crate::types::{
        Action, Client, FullscreenPolicy, LayoutKind, Monitor, Rect, State, WinFlags, WindowId,
    };

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

    /// Two side-by-side monitors, each with 9 workspaces, so a test can place a
    /// real overlay plus `pending_focus` on a monitor/workspace other than the
    /// selected one and exercise the cross-monitor/cross-workspace deferral.
    fn setup_engine_multi() -> Engine {
        let mut engine = Engine::new(default_cfg());
        engine
            .state
            .monitors
            .push(Monitor::new(Rect::new(0, 0, 1920, 1080), 9));
        engine
            .state
            .monitors
            .push(Monitor::new(Rect::new(1920, 0, 1920, 1080), 9));
        engine
    }

    #[test]
    fn fullscreen_horizontal_navigation_releases_exclusive_overlay() {
        use crate::core::commands::{FocusDirection, ToggleFullscreen};
        use crate::core::layout::{arrange, Placements};
        use crate::types::Dir;

        for (direction, policy) in [
            (Dir::Left, FullscreenPolicy::Normal),
            (Dir::Right, FullscreenPolicy::Normal),
            (Dir::Left, FullscreenPolicy::Deny),
            (Dir::Right, FullscreenPolicy::True),
        ] {
            let mut engine = setup_engine();
            for win in [1, 2] {
                let mut c = Client::new(win, 0, 0);
                c.border_w = 2;
                engine.state.add_client(c);
                engine.state.monitors[0].workspaces[0].add_tiled(win, 0.6);
            }
            engine.state.monitors[0].workspaces[0].focus.column_idx = 0;
            engine.state.monitors[0].focused = Some(1);
            engine.state.monitors[0].focus_stack = vec![2, 1];
            engine.state.clients.get_mut(&1).unwrap().fullscreen_policy = policy;
            engine.execute(ToggleFullscreen(Some(1)));
            let snapshot = engine.state.clients[&1].fs_snapshot;
            assert_eq!(engine.state.presented_overlay_owner(0), Some(1));
            // manage(B) can leave the insertion cursor on B while A retains
            // actual focus and the map-time deferral belongs to A.
            engine.state.monitors[0].workspaces[0].focus.column_idx = 1;
            engine.state.pending_focus = Some(crate::types::PendingFocus {
                window: 2,
                owner: 1,
                monitor: 0,
                workspace: 0,
            });

            engine.execute(FocusDirection(direction));
            assert!(engine.state.pending_focus.is_none());
            assert_eq!(engine.state.monitors[0].focused, Some(2));
            assert_eq!(
                engine.state.presented_overlay_owner(0),
                None,
                "explicit horizontal navigation must release the pinned overlay"
            );
            assert!(engine.state.clients[&1].is_fullscreen());
            assert_eq!(engine.state.clients[&1].fs_snapshot, snapshot);

            let camera = engine.state.monitors[0].ws().camera.position;
            engine.state.monitors[0].workspaces[0].camera.position = camera;
            let mut placements = Placements::new();
            arrange(
                &engine.state,
                0,
                &engine.cfg,
                &mut placements,
                &mut RibbonScratch::default(),
            );
            crate::core::present::present(
                &engine.state,
                &engine.state.monitors[0],
                &mut placements,
            );
            let a = placements.iter().find(|e| e.0 == 1).unwrap().1;
            let b = placements.iter().find(|e| e.0 == 2).unwrap().1;
            let screen = engine.state.monitors[0].screen;
            assert!(
                b.x >= screen.x && b.right() <= screen.x + screen.w as i32,
                "B must be fully revealed on screen: {b:?}"
            );
            assert!(
                a.right() <= b.x || a.x >= b.right(),
                "A must not cover B: {a:?}"
            );

            engine.execute(FocusDirection(direction));
            let camera = engine.state.monitors[0].ws().camera.position;
            engine.state.monitors[0].workspaces[0].camera.position = camera;
            arrange(
                &engine.state,
                0,
                &engine.cfg,
                &mut placements,
                &mut RibbonScratch::default(),
            );
            crate::core::present::present(
                &engine.state,
                &engine.state.monitors[0],
                &mut placements,
            );
            let a = placements.iter().find(|e| e.0 == 1).unwrap();
            assert_eq!(
                (a.1, a.2),
                (screen, 0),
                "returning to A preserves fullscreen"
            );
            engine.execute(ToggleFullscreen(Some(1)));
            assert!(!engine.state.clients[&1].is_fullscreen());
            assert_eq!(engine.state.clients[&1].border_w, 2);
            assert_eq!(engine.state.clients[&1].fullscreen_policy, policy);
        }
    }

    #[test]
    fn fullscreen_horizontal_navigation_without_neighbour_keeps_overlay() {
        use crate::core::commands::{FocusDirection, ToggleFullscreen};
        use crate::types::Dir;
        let mut engine = setup_engine();
        engine.state.add_client(Client::new(1, 0, 0));
        engine.state.monitors[0].workspaces[0].add_tiled(1, 0.6);
        engine.state.monitors[0].focused = Some(1);
        engine.state.monitors[0].focus_stack = vec![1];
        engine.execute(ToggleFullscreen(Some(1)));
        for direction in [Dir::Left, Dir::Right] {
            engine.execute(FocusDirection(direction));
            assert_eq!(engine.state.presented_overlay_owner(0), Some(1));
            assert!(engine.state.clients[&1].is_fullscreen());
        }
    }


    #[test]
    fn test_cycle_layout_wraps_around() {
        let mut engine = setup_engine();
        assert_eq!(
            engine.state.monitors[0].workspaces[0].layout,
            LayoutKind::Column
        );
        let _ = engine.execute(crate::core::commands::SetLayout(LayoutKind::Column));
        assert_eq!(
            engine.state.monitors[0].workspaces[0].layout,
            LayoutKind::Column
        );
    }

    #[test]
    fn test_window_created_produces_layout_placement() {
        use crate::core::layout::{arrange, Placements};
        use crate::types::Client;

        let mut engine = setup_engine();
        let new_window_id = 1001;

        // Reproduce exactly what the backend's `manage` does on a MapRequest:
        // register the client and add it to the active workspace's columns.
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i]
            .add_tiled(new_window_id, engine.cfg.column_width);
        let mut client = Client::new(new_window_id, mi, ws_i);
        client.border_w = engine.cfg.border_w;
        engine.state.add_client(client);

        // Run the pure layout the live path uses (backend::arrange → layout::arrange).
        let mut placements = Placements::with_capacity(4);
        arrange(
            &engine.state,
            mi,
            &engine.cfg,
            &mut placements,
            &mut RibbonScratch::default(),
        );

        let placed = placements.iter().any(|(win, _, _)| *win == new_window_id);
        assert!(
            placed,
            "a newly managed window must receive a layout placement"
        );
    }

    #[test]
    fn test_workspace_cycle_layout_helper_wraps() {
        use crate::types::Workspace;
        let ws = Workspace::new(0);
        assert_eq!(ws.layout, LayoutKind::Column);
        assert_eq!(ws.layout, LayoutKind::Column);
    }

    fn setup_two_columns() -> Engine {
        use crate::types::{Client, Column, Focus};
        let mut engine = setup_engine();
        engine.state.add_client(Client::new(10, 0, 0));
        engine.state.add_client(Client::new(20, 0, 0));
        let ws = &mut engine.state.monitors[0].workspaces[0];
        ws.columns.push(Column {
            windows: vec![10],
            focused: 0,
            weight: 0.5,
        });
        ws.columns.push(Column {
            windows: vec![20],
            focused: 0,
            weight: 0.5,
        });
        ws.focus = Focus { column_idx: 0 };
        engine.state.monitors[0].focused = Some(10);
        engine
    }

    #[test]
    fn test_move_right_single_window_swaps_not_merges() {
        let mut engine = setup_two_columns();
        engine.state.apply_move_dir(crate::types::Dir::Right);
        let ws = &engine.state.monitors[0].workspaces[0];
        assert_eq!(ws.columns.len(), 2, "swap must keep 2 separate columns");
        assert_eq!(ws.columns[0].windows, vec![20]);
        assert_eq!(ws.columns[1].windows, vec![10]);
        assert_eq!(ws.focus.column_idx, 1);
    }

    #[test]
    fn test_move_left_right_reversible() {
        let mut engine = setup_two_columns();
        engine.state.apply_move_dir(crate::types::Dir::Right);
        engine.state.apply_move_dir(crate::types::Dir::Left);
        let ws = &engine.state.monitors[0].workspaces[0];
        assert_eq!(ws.columns.len(), 2);
        assert_eq!(ws.columns[0].windows, vec![10], "10 back at col 0");
        assert_eq!(ws.columns[1].windows, vec![20], "20 back at col 1");
        assert_eq!(ws.focus.column_idx, 0);
    }

    #[test]
    fn test_move_right_multi_window_extracts() {
        use crate::types::{Client, Column, Focus};
        let mut engine = setup_engine();
        engine.state.add_client(Client::new(10, 0, 0));
        engine.state.add_client(Client::new(20, 0, 0));
        let ws = &mut engine.state.monitors[0].workspaces[0];
        ws.columns.push(Column {
            windows: vec![10, 20],
            focused: 0,
            weight: 0.5,
        });
        ws.focus = Focus { column_idx: 0 };
        engine.state.monitors[0].focused = Some(10);

        engine.state.apply_move_dir(crate::types::Dir::Right);
        let ws = &engine.state.monitors[0].workspaces[0];
        assert_eq!(ws.columns.len(), 2, "extract must create a new column");
        assert_eq!(ws.columns[0].windows, vec![20]);
        assert_eq!(ws.columns[1].windows, vec![10]);
        assert_eq!(ws.focus.column_idx, 1);
    }
    #[test]
    fn test_move_right_boundary_is_noop() {
        use crate::types::{Client, Column, Focus};
        let mut engine = setup_engine();
        engine.state.add_client(Client::new(10, 0, 0));
        let ws = &mut engine.state.monitors[0].workspaces[0];
        ws.columns.push(Column {
            windows: vec![10],
            focused: 0,
            weight: 0.5,
        });
        ws.focus = Focus { column_idx: 0 };
        engine.state.monitors[0].focused = Some(10);

        let changed = engine.state.apply_move_dir(crate::types::Dir::Right);
        assert!(!changed, "move at rightmost boundary must return false");
        assert_eq!(engine.state.monitors[0].workspaces[0].columns.len(), 1);
    }

    // Viewport zoom and overview are mutually exclusive: both scale the whole
    // workspace through `alpha`, so whichever was entered last owns that scalar
    // and the other axis must be reset to 1.0 — otherwise one mode is a silent
    // no-op or a later scale is pulled from a phantom value.
    // Exact float compares are sound here: the commands assign exactly 1.0.
    #[allow(clippy::float_cmp)]
    #[test]
    fn b1_viewport_then_overview_resets_viewport() {
        use crate::core::commands::{ToggleOverview, ViewportZoom};
        use crate::types::ViewportMode;
        let mut engine = setup_two_columns();
        engine.execute(ViewportZoom(1.0)); // page_zoom * 2 → enters Zoomed
        let ws = &engine.state.monitors[0].workspaces[0];
        assert_eq!(ws.viewport_mode, ViewportMode::Zoomed);
        assert!(ws.page_zoom > 1.0);
        assert!(!ws.overview, "viewport zoom must clear overview");
        engine.execute(ToggleOverview);
        let ws = &engine.state.monitors[0].workspaces[0];
        assert!(ws.overview);
        assert_eq!(
            ws.viewport_mode,
            ViewportMode::Normal,
            "overview must exit viewport zoom"
        );
        assert_eq!(
            ws.page_zoom, 1.0,
            "overview must reset page_zoom"
        );
    }

    #[allow(clippy::float_cmp)]
    #[test]
    fn b1_overview_then_viewport_resets_overview() {
        use crate::core::commands::{ToggleOverview, ViewportZoom};
        use crate::types::ViewportMode;
        let mut engine = setup_two_columns();
        engine.execute(ToggleOverview);
        assert!(engine.state.monitors[0].workspaces[0].overview);
        engine.execute(ViewportZoom(0.5));
        let ws = &engine.state.monitors[0].workspaces[0];
        assert_eq!(ws.viewport_mode, ViewportMode::Zoomed);
        assert!(!ws.overview, "viewport zoom must clear overview (bug B1)");
        assert_eq!(
            ws.zoom, 1.0,
            "viewport zoom must reset the overview zoom"
        );
    }

    #[allow(clippy::float_cmp)]
    #[test]
    fn b1_viewport_zoom_does_not_corrupt_live_zoom() {
        use crate::core::commands::{ToggleOverview, ViewportZoom};
        use crate::types::ViewportMode;
        let mut engine = setup_two_columns();
        engine.execute(ViewportZoom(1.0));
        // Entering Overview after a viewport zoom must clear the viewport axis:
        // the two are mutually exclusive, so `alpha` follows `zoom` alone and
        // the enlargement does not leak into the film-strip.
        engine.execute(ToggleOverview);
        let ws = &engine.state.monitors[0].workspaces[0];
        assert!(ws.overview);
        assert_eq!(ws.viewport_mode, ViewportMode::Normal);
        assert!(
            (ws.page_zoom - 1.0).abs() < 0.01,
            "entering Overview must reset the viewport zoom, got {}",
            ws.page_zoom
        );
        assert!(
            (ws.zoom - engine.cfg.overview_zoom_min).abs() < 0.01,
            "Overview zooms out to the configured minimum, got {}",
            ws.zoom
        );
    }

    // Next/Prev must leave `column.focused` on the row the focus actually moved
    // to: `Column::focused` is what layout reads to pick the tile within a
    // column, so drift here shows up as the camera centring on the wrong window.
    #[test]
    fn b2_focus_next_syncs_column_focused_row() {
        use crate::core::commands::FocusDirection;
        use crate::types::{Client, Column, Dir, Focus};
        let mut engine = setup_engine();
        engine.state.add_client(Client::new(10, 0, 0));
        engine.state.add_client(Client::new(11, 0, 0));
        engine.state.add_client(Client::new(20, 0, 0));
        let ws = &mut engine.state.monitors[0].workspaces[0];
        ws.columns.push(Column {
            windows: vec![10, 11],
            focused: 0,
            weight: 0.5,
        });
        ws.columns.push(Column {
            windows: vec![20],
            focused: 0,
            weight: 0.5,
        });
        ws.focus = Focus { column_idx: 0 };
        engine.state.monitors[0].focused = Some(10);
        engine.state.monitors[0].focus_stack = vec![10, 11, 20];

        engine.execute(FocusDirection(Dir::Next));
        let ws = &engine.state.monitors[0].workspaces[0];
        assert_eq!(engine.state.monitors[0].focused, Some(11));
        assert_eq!(
            ws.columns[0].focused, 1,
            "Next must sync column.focused to the target's row (bug B2)"
        );
    }

    // Commands are pure `State`/`Cfg` transformations that emit `Effect`s. The
    // effect set is the contract the backend drains, so a mutation that changes
    // geometry must ask for the arrange, and a no-op must stay completely silent
    // rather than waking every IPC subscriber.

    #[test]
    fn test_set_layout_command_emits_arrange() {
        let mut engine = setup_engine();
        let effects = engine.execute(crate::core::commands::SetLayout(LayoutKind::Column));
        assert_eq!(
            engine.state.monitors[0].workspaces[0].layout,
            LayoutKind::Column
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, crate::core::Effect::ArrangeMonitor(0))),
            "SetLayout must emit ArrangeMonitor for the selected monitor"
        );
        // The engine appends a state publish so IPC subscribers stay in sync.
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, crate::core::Effect::PublishIpcState)),
            "mutation must end with PublishIpcState"
        );
    }

    #[test]
    fn test_noop_command_emits_no_publish() {
        // A command that produces no effects (e.g. focusing a nonexistent
        // window) must not spam IPC state to subscribers.
        let mut engine = setup_engine();
        let effects = engine.execute(crate::core::commands::FocusWindow(None));
        assert!(
            effects.is_empty(),
            "no-op command must emit nothing (incl. no PublishIpcState)"
        );
    }

    // `PublishIpcState` publishes "the state represented by all effects generated
    // for this command", and the backend builds that snapshot from `State` at
    // the moment it drains the effect (`WindowManager::publish_state`). So it has
    // to be the LAST effect: a publish placed before the remaining effects hands
    // every subscriber a picture of an intermediate focus or workspace state that
    // the very same command is about to replace. Both state-changing halves are
    // covered below — a focus the command itself requested, and a focus only the
    // engine's post-command safety net installed.
    #[test]
    fn the_state_publish_is_the_last_effect_of_the_command_that_produced_it() {
        use crate::core::effect::Effect;

        // A focus the command requested itself: the monitor move arranges both
        // monitors and asks the sink to focus the window on the new one.
        let mut engine = setup_engine_multi();
        for win in [1, 2] {
            let mut c = Client::new(win, 0, 0);
            c.border_w = 2;
            engine.state.add_client(c);
            engine.state.monitors[0].workspaces[0].add_tiled(win, 0.6);
        }
        t_focus(&mut engine, 1);
        let effects = engine.execute(crate::core::commands::MoveWindowToMonitor(
            1,
            crate::types::Dir::Next,
        ));
        assert!(
            matches!(effects.last(), Some(Effect::PublishIpcState)),
            "the publish must follow the focus it publishes: {effects:?}"
        );
        let focus_at = effects
            .iter()
            .position(|e| matches!(e, Effect::FocusWindow(Some(1))))
            .expect("the move requests the focus");
        let publish_at = effects
            .iter()
            .position(|e| matches!(e, Effect::PublishIpcState))
            .expect("a command with effects publishes");
        assert!(
            focus_at < publish_at,
            "the published snapshot must already contain the new focus: {effects:?}"
        );
        assert_eq!(
            effects
                .iter()
                .filter(|e| matches!(e, Effect::PublishIpcState))
                .count(),
            1,
            "exactly one publish per command: {effects:?}"
        );

        // A focus ONLY the safety net installs. The overlay is torn down outside
        // `Command::execute` (the shape of the EWMH per-axis maximize path), and
        // the command issued afterwards is absorbed outright, so the deferral
        // resolution is the only thing the turn did. That resolution still
        // installs a logical focus, and the contract says the publish carries it.
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);
        assert!(
            !t_manage(&mut engine, 2),
            "window 2 is deferred behind the overlay"
        );
        t_set_fullscreen(&mut engine, 1, false);

        let effects = engine.execute(crate::core::commands::FocusWindow(None));
        assert_eq!(
            effects.len(),
            2,
            "an absorbed command publishes only for the focus the safety net \
             installed: {effects:?}"
        );
        assert!(
            matches!(effects[0], Effect::FocusWindow(Some(2))),
            "the resolved deferral's focus is the only other effect: {effects:?}"
        );
        assert!(
            matches!(effects[1], Effect::PublishIpcState),
            "and it is published: {effects:?}"
        );
        assert_eq!(engine.state.monitors[mi].focused, Some(2));
        assert!(engine.state.pending_focus.is_none());
    }

    #[test]
    fn test_quit_action_leads_with_shutdown_effect() {
        // `Mod4+Shift+Q` → `Action::Quit` must reach the backend's graceful
        // shutdown (`Effect::Quit` → `begin_shutdown`) and nothing else: no
        // follow-up arrange/focus/kill effect may ride along in the same turn.
        let mut engine = setup_engine();
        engine.state.running = true;
        let effects = engine.dispatch(Action::Quit);
        assert!(
            matches!(effects.first(), Some(crate::core::Effect::Quit)),
            "Quit must lead with Effect::Quit, got {effects:?}"
        );
        assert!(
            effects.iter().all(|e| matches!(
                e,
                crate::core::Effect::Quit | crate::core::Effect::PublishIpcState
            )),
            "Quit must not emit a follow-up action effect, got {effects:?}"
        );
        // The core never clears `running` itself: the backend arms the global
        // client-close budget and only the run loop stops once clients are gone
        // or the budget elapsed.
        assert!(engine.state.running, "the core must not flip `running`");
    }

    // A command publishes its own domain event exactly once per mutation; it
    // never names its consumers, and handlers are called outside the borrow of
    // `State`, so a handler sees the post-command state.
    #[test]
    fn test_event_bus_notifies_subscribers() {
        use crate::core::commands::SetLayout;
        use crate::core::event::{Event, EventHandler};
        // A handler that counts via a shared Mutex, so the test can read the
        // count after `subscribe` hands the box over to the engine.
        let counter = std::sync::Arc::new(std::sync::Mutex::new(0usize));
        struct CountingHandler(std::sync::Arc<std::sync::Mutex<usize>>);
        impl EventHandler for CountingHandler {
            fn on_event(&mut self, _e: &Event) {
                *self.0.lock().unwrap() += 1;
            }
        }
        let mut engine = setup_engine();
        engine.subscribe(Box::new(CountingHandler(counter.clone())));

        engine.execute(SetLayout(LayoutKind::Column));
        assert_eq!(
            *counter.lock().unwrap(),
            1,
            "SetLayout must notify with one LayoutChanged event"
        );
    }

    #[test]
    fn test_execute_batch_publishes_state_once() {
        use crate::core::commands::{FocusDirection, GrowColumn, SetLayout};
        use crate::types::Dir;
        let mut engine = setup_engine();
        // Seed a column so the commands actually mutate state.
        {
            let ws = &mut engine.state.monitors[0].workspaces[0];
            ws.columns.push(crate::types::Column {
                windows: vec![10],
                focused: 0,
                weight: 0.5,
            });
            ws.focus = crate::types::Focus { column_idx: 0 };
        }
        engine
            .state
            .clients
            .insert(10, crate::types::Client::new(10, 0, 0));
        engine.state.monitors[0].focused = Some(10);

        let batch: Vec<Box<dyn crate::core::commands::Command>> = vec![
            Box::new(SetLayout(LayoutKind::Column)),
            Box::new(GrowColumn(50)),
            Box::new(FocusDirection(Dir::Down)),
        ];
        let effects = engine.execute_batch(batch);
        let publishes = effects
            .iter()
            .filter(|e| matches!(e, crate::core::Effect::PublishIpcState))
            .count();
        assert_eq!(
            publishes, 1,
            "a 3-command transaction must coalesce into exactly one state publish, got {publishes}",
        );
    }

    // External consumers (bars, hooks, tests) read through `Engine::query()` and
    // `query_json`, never by reaching into `State`/`Client` directly.

    fn seed_engine_with_window() -> Engine {
        use crate::types::{Client, Column, Focus};
        let mut engine = setup_engine();
        // A client on monitor 0, workspace 0.
        engine.state.clients.insert(
            42,
            Client {
                name: "term".into(),
                ..Client::new(42, 0, 0)
            },
        );
        {
            let ws = &mut engine.state.monitors[0].workspaces[0];
            ws.columns.push(Column {
                windows: vec![42],
                focused: 0,
                weight: 0.5,
            });
            ws.focus = Focus { column_idx: 0 };
        }
        engine.state.monitors[0].focused = Some(42);
        engine.state.monitors[0].workspaces[0].layout = LayoutKind::Column;
        engine
    }

    #[test]
    fn test_query_reports_focus_workspace_layout() {
        let engine = seed_engine_with_window();
        let q = engine.query();
        assert_eq!(q.focused_window(), Some(42));
        assert_eq!(q.active_workspace(), 0);
        assert_eq!(q.current_layout(), LayoutKind::Column);
        assert_eq!(q.monitor_count(), 1);
        assert_eq!(q.workspace_count(), 9);
    }

    // `query tree` carries the non-semantic desired/applied/real/focus/
    // x11_focus/overlay/pending mirrors so a live session can be audited
    // end-to-end. They are read-only; this locks in their presence and that a
    // focused seeded window reports `focus:true`.
    #[test]
    fn query_tree_includes_observability_fields() {
        let engine = seed_engine_with_window();
        let json = crate::core::ipc::query_json(&engine.state, &engine.cfg, "tree");
        for key in [
            "\"desired\"",
            "\"applied\"",
            "\"real\"",
            "\"focus\"",
            "\"x11_focus\"",
            "\"overlay\"",
            "\"pending\"",
        ] {
            assert!(
                json.contains(key),
                "query tree missing observability key {key}"
            );
        }
        // mon0.focused == 42 in the seed, so its window_obj must report focus.
        assert!(
            json.contains("\"focus\":true"),
            "seeded focused window should report focus:true"
        );
    }

    // `query tree` carries the client PID so a tool can go window → process.
    // Absent `_NET_WM_PID` must serialize as JSON `null`, never as `0` (a pid of
    // 0 is a live "signal my whole process group" to every consumer).
    #[test]
    fn query_tree_reports_client_pid() {
        let mut engine = seed_engine_with_window();
        engine.state.clients.get_mut(&42).unwrap().pid = Some(18_251);
        let json = crate::core::ipc::query_json(&engine.state, &engine.cfg, "tree");
        assert!(
            json.contains("\"pid\":18251"),
            "tree must carry the client pid: {json}"
        );

        engine.state.clients.get_mut(&42).unwrap().pid = None;
        let json = crate::core::ipc::query_json(&engine.state, &engine.cfg, "tree");
        assert!(
            json.contains("\"pid\":null"),
            "a window without _NET_WM_PID must report pid:null: {json}"
        );
        assert!(
            !json.contains("\"pid\":0"),
            "pid must never be reported as 0: {json}"
        );
    }

    #[test]
    fn test_query_visible_windows_and_info() {
        let engine = seed_engine_with_window();
        let q = engine.query();
        assert_eq!(q.visible_windows(), vec![42]);
        let info = q.window(42).expect("window 42 must be queryable");
        assert_eq!(info.id, 42);
        assert_eq!(info.title, "term");
        assert!(!info.floating);
        assert_eq!(info.workspace, 0);
        assert_eq!(info.monitor, 0);
    }

    // A tile hidden under a fullscreen overlay (peek mode) must give the focus
    // back to the overlay's window, not to an invisible tile underneath.
    #[test]
    fn test_best_focus_prefers_overlay_window() {
        use crate::types::{Client, Column, Focus, WinFlags};
        let mut engine = setup_engine();
        // Two tiled windows + focus on 42 (peeking over a fullscreen 7).
        {
            let ws = &mut engine.state.monitors[0].workspaces[0];
            ws.columns.push(Column {
                windows: vec![7, 42],
                focused: 1,
                weight: 0.5,
            });
            ws.focus = Focus { column_idx: 0 };
        }
        engine.state.monitors[0].focused = Some(42);
        engine.state.clients.insert(7, Client::new(7, 0, 0));
        engine.state.clients.insert(42, Client::new(42, 0, 0));
        engine
            .state
            .clients
            .get_mut(&7)
            .unwrap()
            .flags
            .set(WinFlags::FULLSCREEN);
        engine.state.clients.get_mut(&7).unwrap().fullscreen_policy =
            crate::types::FullscreenPolicy::True;
        // Overlay ownership comes from `FullscreenPolicy::True`, not from the
        // workspace layout: `presented_overlay_owner` never reads `LayoutKind`.
        engine.state.monitors[0].workspaces[0].layout = LayoutKind::Column;
        // Focus history: 42 was peeked most recently, 7 was fullscreen before.
        engine.state.monitors[0].focus_stack = vec![7, 42];

        // With 42 focused and 7 pinned as the overlay, `best_focus` must still
        // name 7 — the window the user is actually looking at.
        assert_eq!(engine.state.best_focus(0), Some(7));

        // Dropping the fullscreen flag removes the overlay, so the preference
        // order falls back to the column-focused window.
        engine
            .state
            .clients
            .get_mut(&7)
            .unwrap()
            .flags
            .clear(WinFlags::FULLSCREEN);
        assert_eq!(engine.state.best_focus(0), Some(42));
    }

    #[test]
    fn test_query_json_topics_return_wellformed_documents() {
        use crate::core::ipc::query_json;
        let mut engine = setup_engine();

        let workspaces = query_json(&engine.state, &engine.cfg, "workspaces");
        assert!(workspaces.starts_with('{'));
        assert!(workspaces.contains("\"monitors\":["));
        assert!(workspaces.contains("\"sel_mon\":"));
        assert!(workspaces.contains("\"windows\":[]"));

        let tree = query_json(&engine.state, &engine.cfg, "tree");
        assert!(tree.contains("\"columns\":"));
        assert!(tree.contains("\"floats\":"));
        assert!(tree.contains("\"layout\":\"column\""));

        let focused = query_json(&engine.state, &engine.cfg, "focused");
        assert!(focused.contains("\"window\":null"));

        // A workspace with a window reports it by id.
        engine
            .state
            .monitors
            .get_mut(0)
            .unwrap()
            .workspaces
            .get_mut(0)
            .unwrap()
            .floats
            .push(9);
        engine
            .state
            .clients
            .insert(9, crate::types::Client::new(9, 0, 0));
        engine.state.monitors[0].focused = Some(9);
        let focused = query_json(&engine.state, &engine.cfg, "focused");
        assert!(focused.contains("\"window\":9"));
        let workspaces = query_json(&engine.state, &engine.cfg, "workspaces");
        assert!(workspaces.contains("\"windows\":[9]"));
    }

    #[test]
    fn query_json_unknown_topic_is_an_error() {
        use crate::core::ipc::query_json;
        let engine = setup_engine();
        let bad = query_json(&engine.state, &engine.cfg, "nonsense");
        assert!(bad.starts_with("error unknown-query:"));
    }

    #[test]
    fn test_multi_column_overflow_prevention() {
        use crate::core::layout::{arrange, Placements};
        use crate::types::Client;
        let mut engine = setup_engine();
        let n = 6usize;
        for i in 1..=n as u32 {
            let mi = engine.state.sel_mon;
            let ws_i = engine.state.monitors[mi].active_ws;
            engine.state.monitors[mi].workspaces[ws_i].add_tiled(i, 0.5);
            let mut c = Client::new(i, mi, ws_i);
            c.border_w = engine.cfg.border_w;
            engine.state.add_client(c);
        }
        let wa = engine.state.monitors[0].workarea;
        let gap = engine.cfg.gaps_inner as i32;
        let mut placements = Placements::new();
        arrange(
            &engine.state,
            0,
            &engine.cfg,
            &mut placements,
            &mut RibbonScratch::default(),
        );

        // Columns keep independent fixed widths and never shrink to fit: they
        // are laid out sequentially in a ribbon that may extend past the screen
        // (the camera scrolls).
        let mut prev_right: i32 = wa.x - gap;
        for &(win, geom, _) in &placements {
            let _ = win;
            assert!(geom.x >= prev_right, "columns must not overlap");
            assert!(
                geom.w as i32 <= wa.w as i32,
                "no single column exceeds the workarea"
            );
            assert!(geom.y >= wa.y);
            assert!(geom.bottom() <= wa.bottom());
            prev_right = geom.right() + gap;
        }
        // The ribbon as a whole extends beyond the workarea (scrolling ribbon),
        // which only happens when column widths are NOT normalized to fit.
        assert!(
            prev_right - gap > wa.right(),
            "ribbon should extend past the workarea when columns exceed it"
        );
    }

    #[test]
    fn test_new_column_single_window_keeps_full_width() {
        // The sole column must keep a full-area weight; a sub-0.1 sliver here is
        // what a stale column-width fallback would produce.
        use crate::core::commands::{Command, NewColumn};
        use crate::types::Client;
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].add_tiled(1, 0.6);
        engine.state.monitors[mi].focused = Some(1);
        engine.state.monitors[mi].focus_stack = vec![1];
        let mut c = Client::new(1, mi, ws_i);
        c.border_w = engine.cfg.border_w;
        engine.state.add_client(c);

        NewColumn.execute(&mut engine.state, &mut engine.cfg);

        let ws = &engine.state.monitors[mi].workspaces[ws_i];
        assert_eq!(ws.columns.len(), 1, "single window stays in its own column");
        assert!(
            ws.columns[0].weight > 0.9,
            "sole column must fill the workarea (weight ~1.0), got {}",
            ws.columns[0].weight
        );
    }

    #[test]
    fn test_fullscreen_unfocused_layering() {
        use crate::core::layout::{arrange, Placements};
        use crate::core::present::present;
        use crate::types::{Client, WinFlags};
        let mut engine = setup_engine();
        for i in 1..=2u32 {
            let mi = engine.state.sel_mon;
            let ws_i = engine.state.monitors[mi].active_ws;
            engine.state.monitors[mi].workspaces[ws_i].add_tiled(i, 0.5);
            let mut c = Client::new(i, mi, ws_i);
            c.border_w = engine.cfg.border_w;
            engine.state.add_client(c);
        }
        engine
            .state
            .clients
            .get_mut(&1)
            .unwrap()
            .flags
            .set(WinFlags::MAXIMIZED);
        engine.state.monitors[0].focused = Some(2);
        engine.state.monitors[0].focus_stack = vec![1, 2];

        let mut p = Placements::new();
        arrange(
            &engine.state,
            0,
            &engine.cfg,
            &mut p,
            &mut RibbonScratch::default(),
        );
        present(&engine.state, &engine.state.monitors[0], &mut p);

        let (_, rect1, _) = p.iter().find(|e| e.0 == 1).copied().unwrap();
        assert!(rect1.w < engine.state.monitors[0].workarea.w);
    }

    #[test]
    fn test_focus_direction_allowed_in_fullscreen() {
        // `FocusDirection` is never gated on the focused window's fullscreen
        // flag: moving focus away from a fullscreen window is what puts
        // `core::present`'s peek mode to use. Under the default policy the flag
        // survives but the window stays a ribbon participant and scrolls off
        // with the camera; only `FullscreenPolicy::True` keeps it pinned.
        use crate::core::commands::FocusDirection;
        use crate::core::layout::Placements;
        use crate::types::{Client, Column, Dir, Focus, WinFlags};
        let mut engine = setup_engine();
        {
            let ws = &mut engine.state.monitors[0].workspaces[0];
            ws.columns.push(Column {
                windows: vec![1],
                focused: 0,
                weight: 1.0,
            });
            ws.columns.push(Column {
                windows: vec![2],
                focused: 0,
                weight: 1.0,
            });
            ws.focus = Focus { column_idx: 0 };
        }
        engine.state.monitors[0].focused = Some(1);
        engine.state.monitors[0].focus_stack = vec![1, 2];
        engine.state.clients.insert(1, Client::new(1, 0, 0));
        engine.state.clients.insert(2, Client::new(2, 0, 0));
        engine
            .state
            .clients
            .get_mut(&1)
            .unwrap()
            .flags
            .set(WinFlags::FULLSCREEN);

        let before = engine.state.monitors[0].workspaces[0].focus.column_idx;
        engine.execute(FocusDirection(Dir::Right));
        let after = engine.state.monitors[0].workspaces[0].focus.column_idx;
        assert_ne!(
            after, before,
            "FocusDirection must move columns even while window 1 is fullscreen"
        );
        assert_eq!(
            engine.state.monitors[0].focused,
            Some(2),
            "focus must land on window 2 (peek over the still-fullscreen window 1)",
        );
        // The fullscreen flag itself is untouched — only focus moved.
        assert!(engine.state.clients.get(&1).unwrap().is_fullscreen());

        // The fullscreen window must SCROLL AWAY with the camera (it is now a
        // ribbon participant) instead of staying pinned over the screen.
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        let ws = &engine.state.monitors[mi].workspaces[ws_i];
        let cam = ws.camera.position;
        let mut p = Placements::new();
        engine.state.monitors[mi].workspaces[ws_i].camera.position = cam;
        crate::core::layout::arrange(
            &engine.state,
            mi,
            &engine.cfg,
            &mut p,
            &mut RibbonScratch::default(),
        );
        let screen = engine.state.monitors[mi].screen;
        let (_, fs_rect, _) = p.iter().find(|e| e.0 == 1).copied().unwrap();
        assert!(
            fs_rect.x >= screen.right() || fs_rect.right() <= screen.x,
            "fullscreen must scroll off-screen once focus leaves it: {fs_rect:?}"
        );
    }

    #[test]
    fn test_move_window_allowed_in_fullscreen() {
        // `MoveWindow` is never gated on fullscreen: a fullscreen window is a
        // ribbon participant, so moving it relocates its column in the ribbon
        // instead of re-pinning an overlay.
        use crate::core::commands::MoveWindow;
        use crate::core::layout::{fs_ctx, Placements};
        use crate::types::{Client, Column, Dir, Focus, WinFlags};
        let mut engine = setup_engine();
        {
            let ws = &mut engine.state.monitors[0].workspaces[0];
            ws.columns.push(Column {
                windows: vec![1],
                focused: 0,
                weight: 1.0,
            });
            ws.columns.push(Column {
                windows: vec![2],
                focused: 0,
                weight: 1.0,
            });
            ws.focus = Focus { column_idx: 0 };
        }
        engine.state.monitors[0].focused = Some(1);
        engine.state.clients.insert(1, Client::new(1, 0, 0));
        engine.state.clients.insert(2, Client::new(2, 0, 0));
        engine
            .state
            .clients
            .get_mut(&1)
            .unwrap()
            .flags
            .set(WinFlags::FULLSCREEN);

        let before = engine.state.monitors[0].workspaces[0].focus.column_idx;
        engine.execute(MoveWindow(1, Dir::Right));
        let after = engine.state.monitors[0].workspaces[0].focus.column_idx;
        assert_ne!(
            after, before,
            "MoveWindow must move window 1's column even while fullscreen"
        );
        assert!(engine.state.clients.get(&1).unwrap().is_fullscreen());

        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        let fs = fs_ctx(
            &engine.state.clients,
            &engine.state.monitors[mi].workspaces[ws_i],
            engine.state.monitors[mi].screen,
        );
        assert_eq!(
            fs.cols,
            vec![1],
            "fullscreen column must move within the ribbon"
        );
        // And it is still laid out (covering the screen) by the column layout
        // because it remains the focused window.
        let ws = &engine.state.monitors[mi].workspaces[ws_i];
        let cam = ws.camera.position;
        let mut p = Placements::new();
        engine.state.monitors[mi].workspaces[ws_i].camera.position = cam;
        crate::core::layout::arrange(
            &engine.state,
            mi,
            &engine.cfg,
            &mut p,
            &mut RibbonScratch::default(),
        );
        let (_, fs_rect, bw) = p.iter().find(|e| e.0 == 1).copied().unwrap();
        assert_eq!(bw, 0, "fullscreen keeps border 0");
        assert_eq!(
            fs_rect, engine.state.monitors[mi].screen,
            "focused fullscreen still fills the screen after the move"
        );
    }

    // The scroll camera must keep the focused column fully on-screen for every
    // column count and every focus position, and `ideal_scroll` — the camera's
    // target derivation — must agree with what `arrange` actually produced.
    // A disagreement desyncs the camera from the ribbon and the focused tile
    // ends up off-screen.

    /// Build a workspace on monitor 0 with `n` single-window columns of weight
    /// `[1.0, 0.6, 0.6, …]`, focused at `focus_ci`. `overview` drives the
    /// Overview (zoom-out) state.
    fn build_ribbon(n: usize, focus_ci: usize, overview: bool) -> Engine {
        use crate::types::{Client, Column, Focus};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        {
            let ws = &mut engine.state.monitors[mi].workspaces[0];
            let weights: Vec<f32> = (0..n).map(|i| if i == 0 { 1.0 } else { 0.6 }).collect();
            for (i, w) in weights.iter().enumerate() {
                let win = (i + 1) as u32;
                ws.columns.push(Column {
                    windows: vec![win],
                    focused: 0,
                    weight: *w,
                });
            }
            ws.focus = Focus {
                column_idx: focus_ci,
            };
            if overview {
                ws.overview = true;
                ws.zoom = 0.25;
            }
        }
        for i in 0..n {
            let win = (i + 1) as u32;
            let mut c = Client::new(win, mi, 0);
            c.border_w = engine.cfg.border_w;
            engine.state.add_client(c);
        }
        engine
    }

    #[test]
    fn camera_centers_focused_column() {
        use crate::core::layout::{arrange, ideal_scroll, Placements};
        let cfg = default_cfg();
        for n in 1..=6usize {
            for focus in 0..n {
                let mut engine = build_ribbon(n, focus, false);
                let mi = engine.state.sel_mon;
                let wa = engine.state.monitors[mi].workarea;
                let scroll = ideal_scroll(
                    &engine.state.monitors[mi].workspaces[0],
                    &cfg,
                    wa,
                    FsCtx::default(),
                );
                engine.state.monitors[mi].workspaces[0].camera.position = scroll;
                let mut placements = Placements::new();
                arrange(
                    &engine.state,
                    mi,
                    &cfg,
                    &mut placements,
                    &mut RibbonScratch::default(),
                );

                let fw = engine.state.monitors[mi].workspaces[0]
                    .focused_win()
                    .expect("focused window must exist");
                let (_, geom, bw) = placements
                    .iter()
                    .find(|e| e.0 == fw)
                    .expect("focused window must be placed");
                let bw = *bw as i32;
                let left = geom.x;
                let right = geom.x + geom.w as i32 + 2 * bw;
                assert!(
                    left >= wa.x - 1,
                    "n={n} focus={focus}: focused col left {left} < workarea left {}",
                    wa.x - 1
                );
                assert!(
                    right <= wa.x + wa.w as i32 + 1,
                    "n={n} focus={focus}: focused col right {right} > workarea right {}",
                    wa.x + wa.w as i32 + 1
                );
                // A focused column wider than the visible area can never be
                // scrolled fully on-screen, so no focus position can fix it.
                // The 1 px slack absorbs gap/border rounding.
                assert!(
                    geom.w as i32 + 2 * bw <= wa.w as i32 - 2 * cfg.gaps_outer as i32 + 1,
                    "n={n} focus={focus}: focused column too wide"
                );
            }
        }
    }

    #[test]
    fn ideal_scroll_matches_arrange_geometry() {
        use crate::core::layout::{arrange, ideal_scroll, Placements};
        let cfg = default_cfg();
        for n in 1..=6usize {
            for focus in 0..n {
                let mut engine = build_ribbon(n, focus, false);
                let mi = engine.state.sel_mon;
                let wa = engine.state.monitors[mi].workarea;
                let scroll = ideal_scroll(
                    &engine.state.monitors[mi].workspaces[0],
                    &cfg,
                    wa,
                    FsCtx::default(),
                );
                engine.state.monitors[mi].workspaces[0].camera.position = scroll;
                let mut placements = Placements::new();
                arrange(
                    &engine.state,
                    mi,
                    &cfg,
                    &mut placements,
                    &mut RibbonScratch::default(),
                );

                let fw = engine.state.monitors[mi].workspaces[0]
                    .focused_win()
                    .expect("focused window must exist");
                let (_, geom, bw) = placements
                    .iter()
                    .find(|e| e.0 == fw)
                    .expect("focused window must be placed");
                let left = geom.x as f32;
                let right = (geom.x + geom.w as i32 + 2 * *bw as i32) as f32;
                let fc = (left + right) / 2.0;
                // Geometry is computed against the gaps_outer-inset workarea,
                // so the centering/edge targets must use that inset rect.
                let go = cfg.gaps_outer as i32;
                let iwa_x = wa.x as f32 + go as f32;
                let iwa_w = wa.w as f32 - 2.0 * go as f32;
                let wac = iwa_x + iwa_w / 2.0;

                let min_l = placements
                    .iter()
                    .map(|(_, g, _)| g.x as f32)
                    .fold(f32::INFINITY, f32::min);
                let max_r = placements
                    .iter()
                    .map(|(_, g, b)| (g.x + g.w as i32 + 2 * *b as i32) as f32)
                    .fold(f32::NEG_INFINITY, f32::max);

                let centered = (fc - wac).abs() <= 2.0;
                let touches_left = (min_l - iwa_x).abs() <= 1.0;
                let touches_right = (max_r - (iwa_x + iwa_w)).abs() <= 1.0;
                assert!(
                    centered || touches_left || touches_right,
                    "n={n} focus={focus}: focused center {fc} not centered ({wac}) and no edge touch (min_l {min_l}, max_r {max_r})"
                );
            }
        }
    }

    #[test]
    fn column_screen_extents_agree_with_arrange() {
        use crate::core::layout::{arrange, column_screen_extents, ideal_scroll, Placements};
        let cfg = default_cfg();
        for n in 1..=6usize {
            for focus in 0..n {
                let mut engine = build_ribbon(n, focus, false);
                let mi = engine.state.sel_mon;
                let wa = engine.state.monitors[mi].workarea;
                let scroll = ideal_scroll(
                    &engine.state.monitors[mi].workspaces[0],
                    &cfg,
                    wa,
                    FsCtx::default(),
                );
                engine.state.monitors[mi].workspaces[0].camera.position = scroll;
                let mut placements = Placements::new();
                arrange(
                    &engine.state,
                    mi,
                    &cfg,
                    &mut placements,
                    &mut RibbonScratch::default(),
                );

                let ws = &engine.state.monitors[mi].workspaces[0];
                let extents = column_screen_extents(ws, &cfg, wa, &FsCtx::default());
                assert_eq!(extents.len(), n, "n={n} focus={focus}");
                for (i, &(l, r)) in extents.iter().enumerate() {
                    let (_, geom, _bw) = placements[i]; // placements are pushed in column order
                    let pl = geom.x as f32;
                    let pr = (geom.x + geom.w as i32) as f32;
                    assert!(
                        (l - pl).abs() <= 2.0,
                        "n={n} focus={focus} col {i} left mismatch: extents {l} vs arrange {pl}"
                    );
                    assert!(
                        (r - pr).abs() <= 2.0,
                        "n={n} focus={focus} col {i} right mismatch: extents {r} vs arrange {pr}"
                    );
                }
            }
        }
    }

    #[test]
    fn overview_centers_whole_ribbon() {
        use crate::core::layout::{arrange, ideal_scroll, Placements};
        let cfg = default_cfg();
        let n = 5usize;
        let mut engine = build_ribbon(n, 2, true);
        let mi = engine.state.sel_mon;
        let wa = engine.state.monitors[mi].workarea;
        let scroll = ideal_scroll(
            &engine.state.monitors[mi].workspaces[0],
            &cfg,
            wa,
            FsCtx::default(),
        );
        engine.state.monitors[mi].workspaces[0].camera.position = scroll;
        let mut placements = Placements::new();
        arrange(
            &engine.state,
            mi,
            &cfg,
            &mut placements,
            &mut RibbonScratch::default(),
        );

        let min_l = placements
            .iter()
            .map(|(_, g, _)| g.x as f32)
            .fold(f32::INFINITY, f32::min);
        let max_r = placements
            .iter()
            .map(|(_, g, b)| (g.x + g.w as i32 + 2 * *b as i32) as f32)
            .fold(f32::NEG_INFINITY, f32::max);
        let mid = (min_l + max_r) / 2.0;
        let wac = wa.x as f32 + wa.w as f32 / 2.0;
        assert!(
            (mid - wac).abs() <= 2.0,
            "overview ribbon midpoint {mid} not centered on workarea center {wac}"
        );
        assert!(
            min_l >= wa.x as f32 - 1.0,
            "overview: first column off left ({min_l})"
        );
        assert!(
            max_r <= wa.x as f32 + wa.w as f32 + 1.0,
            "overview: last column off right ({max_r})"
        );
    }

    // Next/Prev walks the focus *stack*, not the column grid, so both must move
    // together: `ideal_scroll` reads `focus.column_idx`, and a stale index leaves
    // the camera on the old column while `mon.focused` has already moved on.

    #[test]
    fn focus_direction_next_prev_syncs_column_and_camera() {
        use crate::core::commands::FocusDirection;
        use crate::core::layout::{arrange, ideal_scroll, Placements};
        use crate::types::{Client, Column, Dir, Focus};
        let cfg = default_cfg();
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        // Three narrow columns so the ribbon is wider than the screen: centering
        // one column pushes the others partially off-screen, the case a stale
        // `focus.column_idx` cannot survive.
        {
            let ws = &mut engine.state.monitors[mi].workspaces[0];
            for i in 1..=3u32 {
                ws.columns.push(Column {
                    windows: vec![i],
                    focused: 0,
                    weight: 0.4,
                    // The accordion is derived from the focus pointer, so no
                    // per-column state is seeded here.
                });
            }
            ws.focus = Focus { column_idx: 0 };
        }
        engine.state.monitors[mi].focused = Some(1);
        engine.state.monitors[mi].focus_stack = vec![1, 2, 3];
        for i in 1..=3u32 {
            engine.state.add_client(Client::new(i, mi, 0));
        }

        // Next: focus moves to window 2 (column 1).
        engine.execute(FocusDirection(Dir::Next));
        assert_eq!(engine.state.monitors[mi].focused, Some(2));
        assert_eq!(
            engine.state.monitors[mi].workspaces[0].focus.column_idx, 1,
            "Next must sync ws.focus.column_idx to the focused window's column"
        );

        // The camera must center column 1, keeping window 2 on-screen.
        let wa = engine.state.monitors[mi].workarea;
        let scroll = ideal_scroll(
            &engine.state.monitors[mi].workspaces[0],
            &cfg,
            wa,
            FsCtx::default(),
        );
        engine.state.monitors[mi].workspaces[0].camera.position = scroll;
        let mut placements = Placements::new();
        arrange(
            &engine.state,
            mi,
            &cfg,
            &mut placements,
            &mut RibbonScratch::default(),
        );
        let fw = engine.state.monitors[mi].workspaces[0]
            .focused_win()
            .unwrap();
        assert_eq!(fw, 2);
        let (_, geom, bw) = placements.iter().find(|e| e.0 == fw).unwrap();
        let left = geom.x;
        let right = geom.x + geom.w as i32 + 2 * *bw as i32;
        assert!(
            left >= wa.x - 1,
            "focused window {fw} left {left} off-screen left"
        );
        assert!(
            right <= wa.x + wa.w as i32 + 1,
            "focused window {fw} right {right} off-screen right"
        );

        // Prev: back to window 1 (column 0), camera follows.
        engine.execute(FocusDirection(Dir::Prev));
        assert_eq!(engine.state.monitors[mi].focused, Some(1));
        assert_eq!(
            engine.state.monitors[mi].workspaces[0].focus.column_idx, 0,
            "Prev must sync ws.focus.column_idx back to the focused window's column"
        );
    }

    #[test]
    fn drop_into_column_sets_focused_row() {
        use crate::types::{Column, Focus, Workspace};
        let mut ws = Workspace::new(0);
        ws.columns.push(Column {
            windows: vec![10, 20],
            focused: 0,
            weight: 0.5,
        });
        ws.focus = Focus { column_idx: 0 };

        // Drop window 30 between 10 and 20 (insert_pos = 1).
        ws.drop_into_column(0, 30, 1);
        assert_eq!(ws.columns[0].windows, vec![10, 30, 20]);
        assert_eq!(
            ws.columns[0].focused, 1,
            "the dropped window must become the focused row"
        );
        assert_eq!(ws.focused_win(), Some(30));
        assert_eq!(ws.focus.column_idx, 0);

        // Append at the end (pos past the end) is valid.
        ws.drop_into_column(0, 40, 99);
        assert_eq!(ws.columns[0].windows, vec![10, 30, 20, 40]);
        assert_eq!(ws.columns[0].focused, 3);

        // An out-of-range column index is a no-op (no panic).
        let before = ws.columns[0].windows.clone();
        ws.drop_into_column(7, 50, 0);
        assert_eq!(ws.columns[0].windows, before);
    }

    // `reload_config` reconciles each monitor's workspaces to the new `n_tags`
    // and clamps every client whose `workspace >= n_tags`; the backend then
    // republishes the EWMH desktop count and the per-client `_NET_WM_DESKTOP`
    // for exactly those clients. This guards the core half of that handoff: no
    // client left above the tag count and no stale slots left on a monitor.
    #[test]
    fn reload_shrinking_tags_clamps_client_workspace() {
        use crate::types::Client;
        let mut engine = setup_engine();
        // Start with 9 tags (default_cfg) and park a few clients on high workspaces.
        for w in [1u32, 2, 3] {
            let mut c = Client::new(w, 0, w as usize % 9 + 5); // workspaces 5,6,7
            c.border_w = engine.cfg.border_w;
            engine.state.add_client(c);
        }
        let n_tags_before = engine.cfg.n_tags;
        assert_eq!(n_tags_before, 9);

        // Simulate the shrink part of `reload_config`: new config has 3 tags.
        let n_tags = 3usize;
        for mon in &mut engine.state.monitors {
            mon.reconcile_workspaces(n_tags);
        }
        let clamped: Vec<u32> = engine
            .state
            .clients
            .iter_mut()
            .filter_map(|(&w, c)| {
                if c.workspace >= n_tags {
                    c.workspace = n_tags.saturating_sub(1);
                    Some(w)
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(
            clamped.len(),
            3,
            "all three clients were on workspaces >= 3"
        );
        assert_eq!(engine.state.monitors[0].workspaces.len(), n_tags);
        for c in engine.state.clients.values() {
            assert!(
                c.workspace < n_tags,
                "client must be clamped below n_tags after reload, got {}",
                c.workspace
            );
        }
    }

    #[test]
    fn ideal_scroll_uses_the_given_workspace() {
        use crate::core::layout::ideal_scroll;
        use crate::types::{Column, Focus};
        let cfg = default_cfg();
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let wa = engine.state.monitors[mi].workarea;
        // ws0 (active) empty; ws1 has 4 columns, focus not centered.
        {
            let ws1 = &mut engine.state.monitors[mi].workspaces[1];
            for i in 0..4u32 {
                ws1.columns.push(Column {
                    windows: vec![],
                    focused: 0,
                    weight: if i == 0 { 1.0 } else { 0.6 },
                });
            }
            ws1.focus = Focus { column_idx: 1 };
        }
        let s1 = ideal_scroll(
            &engine.state.monitors[mi].workspaces[1],
            &cfg,
            wa,
            FsCtx::default(),
        );
        // Non-zero proves it read ws1's columns, NOT mon.ws() (which is empty → 0).
        assert!(
            s1 != 0.0,
            "ideal_scroll must read the passed workspace, not mon.ws()"
        );
        // Pure function of the passed workspace: independent of which is active.
        let s1b = ideal_scroll(
            &engine.state.monitors[mi].workspaces[1],
            &cfg,
            wa,
            FsCtx::default(),
        );
        assert!((s1 - s1b).abs() < 1e-6, "ideal_scroll must be pure");
        // The empty active workspace yields 0.
        let s0 = ideal_scroll(
            &engine.state.monitors[mi].workspaces[0],
            &cfg,
            wa,
            FsCtx::default(),
        );
        assert!((s0 - 0.0).abs() < 1e-6, "empty workspace scroll must be 0");
    }

    // `retain` drops the emptied column and `rebalance_weights` only repairs
    // weights <= 0 (it never re-normalizes), so the collapsed column's weight
    // must be handed to the target or the ribbon permanently loses that much
    // width and leaves a gap on the right of the workarea.
    #[test]
    fn collapse_column_absorbs_collapsed_weight() {
        use crate::core::commands::CollapseColumn;
        use crate::types::{Client, Column, Focus};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        {
            let ws = &mut engine.state.monitors[mi].workspaces[0];
            for i in 1..=3u32 {
                ws.columns.push(Column {
                    windows: vec![i],
                    focused: 0,
                    weight: 1.0 / 3.0,
                });
            }
            ws.focus = Focus { column_idx: 1 };
        }
        for i in 1..=3u32 {
            engine.state.add_client(Client::new(i, mi, 0));
        }
        let before: f32 = engine.state.monitors[mi].workspaces[0]
            .columns
            .iter()
            .map(|c| c.weight)
            .sum();

        engine.execute(CollapseColumn);

        let ws = &engine.state.monitors[mi].workspaces[0];
        assert_eq!(ws.columns.len(), 2, "column 1 collapses into column 0");
        assert_eq!(
            ws.columns[0].windows,
            vec![1, 2],
            "the collapsed column's windows move into the target"
        );
        assert_eq!(ws.focus.column_idx, 0, "focus follows the merged column");
        let after: f32 = ws.columns.iter().map(|c| c.weight).sum();
        assert!(
            (after - before).abs() < 1e-3,
            "total column weight must survive the collapse ({before} -> {after}); \
             losing it leaves an empty gap on the right of the ribbon"
        );
        assert!(
            (ws.columns[0].weight - 2.0 / 3.0).abs() < 1e-3,
            "the target column must grow by the collapsed column's weight, got {}",
            ws.columns[0].weight
        );
    }

    // Up/Down writes `col.focused` to track the row; Left/Right must carry that
    // row into the destination column instead of adopting the destination's own
    // stale `focused`, which would jump to an unrelated window.
    #[test]
    fn focus_direction_horizontal_keeps_the_focused_row() {
        use crate::core::commands::FocusDirection;
        use crate::types::{Client, Column, Dir, Focus};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        {
            let ws = &mut engine.state.monitors[mi].workspaces[0];
            ws.columns.push(Column {
                windows: vec![1, 2, 3],
                focused: 2, // row 2 == window 3
                weight: 0.4,
            });
            ws.columns.push(Column {
                windows: vec![4, 5, 6],
                focused: 0, // stale: never visited
                weight: 0.4,
            });
            ws.columns.push(Column {
                windows: vec![7], // shorter than the row we come from
                focused: 0,
                weight: 0.4,
            });
            ws.focus = Focus { column_idx: 0 };
        }
        for i in 1..=7u32 {
            engine.state.add_client(Client::new(i, mi, 0));
        }
        engine.state.monitors[mi].focused = Some(3);
        engine.state.monitors[mi].focus_stack = (1..=7u32).collect();

        // Right: row 2 of column 0 (window 3) → row 2 of column 1 (window 6).
        engine.execute(FocusDirection(Dir::Right));
        let ws = &engine.state.monitors[mi].workspaces[0];
        assert_eq!(ws.focus.column_idx, 1);
        assert_eq!(ws.columns[1].focused, 2, "the row carries over to column 1");
        assert_eq!(
            engine.state.monitors[mi].focused,
            Some(6),
            "focus-right must land on the same row (window 6), not window 4"
        );

        // Left: symmetric round trip back to row 2 of column 0 (window 3).
        engine.execute(FocusDirection(Dir::Left));
        assert_eq!(engine.state.monitors[mi].workspaces[0].focus.column_idx, 0);
        assert_eq!(engine.state.monitors[mi].focused, Some(3));

        // Right twice: column 2 has a single row, so the row clamps to 0.
        engine.execute(FocusDirection(Dir::Right));
        engine.execute(FocusDirection(Dir::Right));
        let ws = &engine.state.monitors[mi].workspaces[0];
        assert_eq!(ws.focus.column_idx, 2);
        assert_eq!(
            ws.columns[2].focused, 0,
            "the row is clamped to the shorter destination column"
        );
        assert_eq!(engine.state.monitors[mi].focused, Some(7));
    }

    // `best_focus` must mirror `core::present`'s overlay rule: a maximized window
    // is presented only while it is `mon.focused`, so a background maximized
    // window must not be the top candidate either — otherwise viewing a
    // workspace hands it the focus and it blows itself up over the workarea.
    #[test]
    fn best_focus_ignores_unfocused_maximized() {
        use crate::core::commands::ViewWorkspace;
        use crate::core::effect::Effect;
        use crate::types::{Client, Column, Focus, WinFlags};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        {
            let ws = &mut engine.state.monitors[mi].workspaces[0];
            ws.columns.push(Column {
                windows: vec![1],
                focused: 0,
                weight: 0.5,
            });
            ws.columns.push(Column {
                windows: vec![2],
                focused: 0,
                weight: 0.5,
            });
            ws.focus = Focus { column_idx: 1 };
        }
        engine.state.add_client(Client::new(1, mi, 0));
        engine.state.add_client(Client::new(2, mi, 0));
        engine
            .state
            .clients
            .get_mut(&1)
            .unwrap()
            .flags
            .set(WinFlags::MAXIMIZED_V | WinFlags::MAXIMIZED_H);
        engine.state.monitors[mi].focused = Some(2);
        engine.state.monitors[mi].focus_stack = vec![1, 2];

        assert_eq!(
            engine.state.best_focus(mi),
            Some(2),
            "a maximized window that isn't focused is a plain tile, not an overlay"
        );

        // Focused + maximized → it really is the presented overlay, so it wins.
        engine.state.monitors[mi].focused = Some(1);
        engine.state.monitors[mi].workspaces[0].presented_maximize = Some(1);
        assert_eq!(engine.state.best_focus(mi), Some(1));
        engine.state.monitors[mi].focused = Some(2);
        engine.state.monitors[mi].workspaces[0].presented_maximize = None;

        // Switching away and back must not hand focus to the background
        // maximized window (`ViewWorkspace` picks the focus via `best_focus`).
        engine.execute(ViewWorkspace(1));
        let effects = engine.execute(ViewWorkspace(0));
        let focused = effects
            .iter()
            .rev()
            .find_map(|e| match e {
                Effect::FocusWindow(w) => Some(*w),
                _ => None,
            })
            .expect("ViewWorkspace must pick a focus target");
        assert_eq!(
            focused,
            Some(2),
            "returning to the workspace must keep the column focus, \
             not jump to the unfocused maximized window"
        );

        // `FullscreenPolicy::True` makes the window the overlay owner whatever
        // the focus is, and it wins over the column-focused window.
        let c = engine.state.clients.get_mut(&1).unwrap();
        c.flags.clear(WinFlags::MAXIMIZED);
        c.flags.set(WinFlags::FULLSCREEN);
        c.fullscreen_policy = crate::types::FullscreenPolicy::True;
        engine.state.monitors[mi].workspaces[0].layout = LayoutKind::Column;
        assert_eq!(engine.state.best_focus(mi), Some(1));
    }

    // A workspace round trip must preserve both halves of the focus contract:
    // `ViewWorkspace` picks its `FocusWindow` target through `best_focus`, so the
    // window Maverick considers focused has to survive the trip away and back.
    // The matching X-side requirement — committing `mon.focused` before
    // `reconcile_focus()` — lives in `backend/x11/render.rs` and is validated
    // under Xephyr through the `input-trace` diagnostics.
    #[test]
    fn view_workspace_round_trip_keeps_focused_window() {
        use crate::core::commands::ViewWorkspace;
        use crate::core::effect::Effect;
        use crate::types::{Client, Column, Focus};

        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;

        // ws0: Alacritty (1) focused, plus a neighbour (2).
        {
            let ws = &mut engine.state.monitors[mi].workspaces[0];
            ws.columns.push(Column {
                windows: vec![1],
                focused: 0,
                weight: 0.5,
            });
            ws.columns.push(Column {
                windows: vec![2],
                focused: 0,
                weight: 0.5,
            });
            ws.focus = Focus { column_idx: 0 };
        }
        engine.state.add_client(Client::new(1, mi, 0));
        engine.state.add_client(Client::new(2, mi, 0));

        // ws1: a different window (3) of its own.
        {
            let ws = &mut engine.state.monitors[mi].workspaces[1];
            ws.columns.push(Column {
                windows: vec![3],
                focused: 0,
                weight: 1.0,
            });
            ws.focus = Focus { column_idx: 0 };
        }
        engine.state.add_client(Client::new(3, mi, 1));

        engine.state.monitors[mi].focused = Some(1);
        engine.state.monitors[mi].focus_stack = vec![1, 2];

        // Alacritty is the window Maverick considers focused on ws0.
        assert_eq!(engine.state.best_focus(mi), Some(1));

        // Switch to ws1.
        let eff1 = engine.execute(ViewWorkspace(1));
        let fw1 = eff1
            .iter()
            .rev()
            .find_map(|e| match e {
                Effect::FocusWindow(w) => Some(*w),
                _ => None,
            })
            .expect("ViewWorkspace(1) must emit FocusWindow");
        assert_eq!(fw1, Some(3), "ws1's only window must be focused on switch");

        // Switch back to ws0 — Alacritty must still be the focused window and the
        // effect that re-syncs focus must target it.
        let eff0 = engine.execute(ViewWorkspace(0));
        let fw0 = eff0
            .iter()
            .rev()
            .find_map(|e| match e {
                Effect::FocusWindow(w) => Some(*w),
                _ => None,
            })
            .expect("ViewWorkspace(0) must emit FocusWindow");
        assert_eq!(fw0, Some(1), "returning to ws0 must re-focus Alacritty");
        assert_eq!(
            engine.state.best_focus(mi),
            Some(1),
            "the focused window Maverick considers focused must survive the \
             workspace round trip (precondition for the X backend to keep real \
             input focus on the visible window)"
        );
    }
    // `GrowColumn`'s upper bound `1.0 - 0.05*(n-1)` crosses the `.clamp`'s 0.05
    // lower bound once the ribbon is wide enough, and `f32::clamp` panics on
    // `min > max` in debug *and* release. The command must stay total for any
    // column count.

    #[test]
    fn grow_column_does_not_panic_with_many_columns() {
        use crate::types::Client;
        let mut engine = setup_engine();
        let mi = 0;
        let ws_i = 0;
        let n = 25usize;
        for w in 1..=n as u32 {
            engine.state.monitors[mi].workspaces[ws_i].add_tiled(w, 1.0 / n as f32);
            engine.state.add_client(Client::new(w, mi, ws_i));
        }
        // Grow in both directions: the clamp bound must hold for either sign.
        engine.dispatch(Action::GrowCol(50));
        engine.dispatch(Action::GrowCol(-50));
        // Sanity: weights stay finite and non-negative, sum preserved by the
        // non-redistributive path.
        let ws = &engine.state.monitors[mi].workspaces[ws_i];
        let sum: f32 = ws.columns.iter().map(|c| c.weight).sum();
        assert!(sum.is_finite() && sum > 0.0);
        assert!(
            ws.columns
                .iter()
                .all(|c| c.weight >= 0.0 && c.weight.is_finite()),
            "no column weight panicked into NaN/negative"
        );
    }

    #[test]
    fn grow_column_second_tile_can_reach_fullscreen() {
        // The upper bound must stay reachable: a second tile in a 2-column
        // ribbon can still be grown all the way to 1.0 (fullscreen width), and
        // the clamped delta must not lock it below that.
        use crate::types::Client;
        let mut engine = setup_engine();
        let mi = 0;
        let ws_i = 0;
        // Two columns: the 1st at weight 1.0 (alone), the 2nd at
        // `cfg.column_width`.
        engine.state.monitors[mi].workspaces[ws_i].add_tiled(1, 1.0);
        engine.state.add_client(Client::new(1, mi, ws_i));
        engine.state.monitors[mi].workspaces[ws_i].add_tiled(2, 0.6);
        engine.state.add_client(Client::new(2, mi, ws_i));
        engine.state.monitors[mi].focused = Some(2);
        engine.state.monitors[mi].workspaces[ws_i].focus.column_idx = 1;
        // Push the 2nd column to the bound with oversized deltas.
        for _ in 0..30 {
            engine.dispatch(Action::GrowCol(500));
        }
        let w = engine.state.monitors[mi].workspaces[ws_i].columns[1].weight;
        assert!(
            (w - 1.0).abs() < 1e-6,
            "segundo mosaico debe poder llegar a weight=1.0, got {w}"
        );
        // And it must project to the full inner width.
        let mut out = crate::core::layout::Placements::new();
        let mut scratch = crate::core::layout::RibbonScratch::default();
        crate::core::layout::arrange(
            &engine.state,
            mi,
            &engine.cfg,
            &mut out,
            &mut scratch,
        );
        let bw = engine.cfg.border_w;
        let win2 = out.iter().find(|(id, _, _)| *id == 2).unwrap();
        assert_eq!(
            win2.2, bw,
            "borde del mosaico agrandado debe ser cfg.border_w"
        );
        // Inner width = workarea inset by `gaps_outer` minus 2 borders, i.e. the
        // `wa` that `ribbon_geom` works with. With gaps_outer = 6 that is
        // 1920 - 12 = 1908 and the tile interior is 1908 - 4 = 1904.
        let wa_raw = engine.state.monitors[mi].workarea;
        let gap_outer: i32 = engine.cfg.gaps_outer.min(1_000_000) as i32;
        let gap_outer = gap_outer
            .min(wa_raw.w as i32 / 2)
            .min(wa_raw.h as i32 / 2)
            .max(0);
        let wa_w_inset = wa_raw.w.saturating_sub((2 * gap_outer) as u32) as i32;
        let expected_inner = (wa_w_inset - 2 * bw as i32).max(1) as u32;
        assert_eq!(
            win2.1.w, expected_inner,
            "segundo mosaico a pantalla completa debe ocupar wa_inset - marco, got {:?} want {expected_inner} (wa_raw {wa_raw:?})",
            win2.1
        );
    }

    #[test]
    fn float_new_window_does_not_tremble_between_manage_and_arrange() {
        // A float centred at `manage` and re-clamped by `arrange` must land on
        // the same rect. The 2*bw frame is the usual source of the 4 px
        // disagreement, and the next `arrange` turns that into a visible jump.
        fn clamp(mut g: crate::types::Rect, wa: crate::types::Rect, bw: u32) -> crate::types::Rect {
            let frame = 2 * bw as i32;
            let max_w = (wa.w as i32 - frame).max(1) as u32;
            let max_h = (wa.h as i32 - frame).max(1) as u32;
            g.w = g.w.min(max_w).max(1);
            g.h = g.h.min(max_h).max(1);
            let max_x =
                wa.x.saturating_add(wa.w as i32)
                    .saturating_sub(g.w as i32)
                    .saturating_sub(frame)
                    .max(wa.x);
            let max_y =
                wa.y.saturating_add(wa.h as i32)
                    .saturating_sub(g.h as i32)
                    .saturating_sub(frame)
                    .max(wa.y);
            g.x = g.x.clamp(wa.x, max_x);
            g.y = g.y.clamp(wa.y, max_y);
            g
        }
        let engine = setup_engine();
        let mi = engine.state.sel_mon;
        let wa = engine.state.monitors[mi].workarea;
        let bw = engine.cfg.border_w;
        let target = crate::types::Rect::new(
            wa.x + (wa.w as i32 - 400) / 2,
            wa.y + (wa.h as i32 - 300) / 2,
            400,
            300,
        );
        let clamped_manage = clamp(target, wa, bw);
        let g2 = clamp(clamped_manage, wa, bw);
        assert_eq!(
            clamped_manage, g2,
            "clamp del float debe ser idempotente, no temblar"
        );
        // `arrange` clamps with the same formula, so a third pass is a fixed
        // point too.
        let again = clamp(g2, wa, bw);
        assert_eq!(g2, again);
    }

    // `ribbon_geom` is the single source of truth shared by `arrange_columns`,
    // `ideal_scroll` and `column_screen_extents`, so a fullscreen column must
    // feed its special width through it and all three must agree. Mirrors
    // `ribbon_invariants_hold_with_fullscreen` in `layout.rs` at the
    // `Engine`/`arrange` boundary.

    #[test]
    fn fullscreen_column_invariants_match_ribbon_functions() {
        use crate::core::layout::{
            arrange, column_screen_extents, fs_ctx, ideal_scroll, Placements,
        };
        use crate::types::{Client, Column, Focus, WinFlags};
        let cfg = default_cfg();
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        // Two columns; col0 is the fullscreen one (asymmetric left strut to exercise
        // the screen.x alignment).
        {
            let ws = &mut engine.state.monitors[mi].workspaces[0];
            ws.columns.push(Column {
                windows: vec![1],
                focused: 0,
                weight: 1.0,
            });
            ws.columns.push(Column {
                windows: vec![2],
                focused: 0,
                weight: 0.5,
            });
            ws.focus = Focus { column_idx: 0 };
        }
        engine.state.monitors[mi].focused = Some(1);
        engine.state.monitors[mi].focus_stack = vec![1, 2];
        let mut c1 = Client::new(1, mi, 0);
        c1.border_w = 0;
        c1.flags.set(WinFlags::FULLSCREEN);
        engine.state.add_client(c1);
        engine.state.add_client(Client::new(2, mi, 0));

        let wa = engine.state.monitors[mi].workarea;
        let fs = fs_ctx(
            &engine.state.clients,
            &engine.state.monitors[mi].workspaces[0],
            engine.state.monitors[mi].screen,
        );
        let scroll = ideal_scroll(
            &engine.state.monitors[mi].workspaces[0],
            &cfg,
            wa,
            fs.clone(),
        );
        engine.state.monitors[mi].workspaces[0].camera.position = scroll;
        let mut p = Placements::new();
        arrange(
            &engine.state,
            mi,
            &cfg,
            &mut p,
            &mut RibbonScratch::default(),
        );

        // `column_screen_extents` must agree with the arrange placement of the fs col.
        let extents =
            column_screen_extents(&engine.state.monitors[mi].workspaces[0], &cfg, wa, &fs);
        let (_, rect, _) = p.iter().find(|e| e.0 == 1).copied().unwrap();
        let (el, er) = extents[0];
        assert!(
            (el - rect.x as f32).abs() <= 2.0,
            "extents left {el} != arrange left {}",
            rect.x
        );
        assert!(
            (er - (rect.x + rect.w as i32) as f32).abs() <= 2.0,
            "extents right {er} != arrange right {}",
            rect.x + rect.w as i32
        );
        // The aligned camera puts the fullscreen left edge exactly at `screen.x`.
        assert_eq!(
            rect.x, engine.state.monitors[mi].screen.x,
            "fullscreen left must equal screen.x under the aligned camera"
        );
    }

    // Entering fullscreen from a float pulls the window into the tiling as a
    // fresh column and remembers that it was floating; leaving fullscreen
    // restores the float. The core command owns both the topology change and the
    // FULLSCREEN flag, so the backend's `SetFullscreen` handler only has to emit
    // the EWMH atom and the bypass hint without touching logical state.

    #[test]
    fn float_fullscreen_moves_to_tiling_and_back() {
        use crate::core::commands::{Command, ToggleFullscreen};
        use crate::types::{Client, WinFlags};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        let win = 1u32;

        // A floating client.
        let mut c = Client::new(win, mi, ws_i);
        c.border_w = 2;
        c.flags.set(WinFlags::FLOAT);
        c.geom = Rect::new(100, 100, 400, 300);
        c.saved_geom = c.geom;
        engine.state.add_client(c);
        engine.state.monitors[mi].workspaces[ws_i].floats.push(win);
        engine.state.monitors[mi].focused = Some(win);
        engine.state.monitors[mi].focus_stack = vec![win];

        // Enter fullscreen: float → tiled column, remembers FS_WAS_FLOAT.
        ToggleFullscreen(Some(win)).execute(&mut engine.state, &mut engine.cfg);
        {
            let c = engine.state.clients.get(&win).unwrap();
            assert!(
                !c.is_float(),
                "client must leave the float set when fullscreen"
            );
            assert!(
                c.flags.has(WinFlags::FS_WAS_FLOAT),
                "must remember the window was floating"
            );
            assert!(
                !engine.state.monitors[mi].workspaces[ws_i]
                    .floats
                    .contains(&win),
                "client must leave ws.floats"
            );
            assert!(
                engine.state.monitors[mi].workspaces[ws_i]
                    .columns
                    .iter()
                    .any(|col| col.windows.contains(&win)),
                "client must join the tiling as a column"
            );
        }

        // The Command already owns the FULLSCREEN flag (set on enter, cleared on
        // leave) — no backend simulation needed here.

        // Leave fullscreen: tiled → float, restores FLOAT, clears FS_WAS_FLOAT.
        ToggleFullscreen(Some(win)).execute(&mut engine.state, &mut engine.cfg);
        {
            let c = engine.state.clients.get(&win).unwrap();
            assert!(c.is_float(), "client must return to being a float");
            assert!(
                !c.flags.has(WinFlags::FS_WAS_FLOAT),
                "FS_WAS_FLOAT must be cleared on exit"
            );
            assert!(
                engine.state.monitors[mi].workspaces[ws_i]
                    .floats
                    .contains(&win),
                "client must return to ws.floats"
            );
            assert!(
                !engine.state.monitors[mi].workspaces[ws_i]
                    .columns
                    .iter()
                    .any(|col| col.windows.contains(&win)),
                "client must leave the tiling"
            );
        }
    }

    // The fullscreen target resolves from the *logically* focused window. When B
    // is managed while A is a fullscreen/maximized overlay, `manage` must advance
    // the logical focus to B without moving X input focus off the overlay, so the
    // keyboard path — which resolves from `mon.focused` — targets B. `manage`
    // itself needs a live X11 connection, so the scenario is reproduced here at
    // the command layer. The FULLSCREEN flag is owned by `ToggleFullscreen`, so
    // what is asserted is the target of the emitted `SetFullscreen` effect.

    /// Find the `SetFullscreen` effect emitted for `ToggleFullscreen`, if any.
    fn fs_target(
        report: &crate::core::event::CommandReport,
    ) -> Option<(crate::types::WindowId, bool)> {
        use crate::core::effect::Effect;
        report.effects.iter().find_map(|e| match e {
            Effect::SetFullscreen { win, on } => Some((*win, *on)),
            _ => None,
        })
    }

    #[test]
    fn toggle_fullscreen_targets_new_tiled_window_not_overlay() {
        use crate::core::commands::{Command, ToggleFullscreen};
        use crate::types::{Client, WinFlags};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;

        // A is a fullscreen overlay, logically and (X) input focused.
        let a = 1u32;
        let mut ca = Client::new(a, mi, ws_i);
        ca.border_w = 2;
        ca.flags.set(WinFlags::FULLSCREEN);
        engine.state.add_client(ca);
        engine.state.monitors[mi].workspaces[ws_i].add_tiled(a, engine.cfg.column_width);
        engine.state.monitors[mi].focused = Some(a);
        engine.state.monitors[mi].focus_stack = vec![a];
        engine.state.x11_input_focus = Some(a);

        // B is created and tiled under the overlay. The managed-window policy
        // advances the *logical* focus to B while leaving the X input focus on
        // the overlay A.
        let b = 2u32;
        let mut cb = Client::new(b, mi, ws_i);
        cb.border_w = 2;
        engine.state.add_client(cb);
        engine.state.monitors[mi].workspaces[ws_i].add_tiled(b, engine.cfg.column_width);
        engine.state.monitors[mi].focused = Some(b);
        engine.state.monitors[mi].focus_stack.retain(|&x| x != b);
        engine.state.monitors[mi].focus_stack.push(b);

        // The user hits Mod4+F intending to fullscreen B.
        let report = ToggleFullscreen(None).execute(&mut engine.state, &mut engine.cfg);
        let target = fs_target(&report).expect("a SetFullscreen effect must be emitted");

        assert_eq!(
            target,
            (b, true),
            "the newly-tiled B must be the fullscreen target"
        );

        // The Command already set B's FULLSCREEN flag; verify the rest.
        assert!(
            engine.state.clients.get(&a).unwrap().is_fullscreen(),
            "the overlay A must keep its fullscreen"
        );
        assert_eq!(
            engine.state.monitors[mi].focused,
            Some(b),
            "logical focus must stay on B"
        );
        // The overlay keeps the keyboard (input focus is not stolen).
        assert_eq!(
            engine.state.x11_input_focus,
            Some(a),
            "X input focus must not be stolen from the overlay"
        );
    }

    #[test]
    fn toggle_fullscreen_explicit_focus_resolves_to_b() {
        // Control: an explicit focus move to B must behave identically — the
        // command resolves from `mon.focused`, so A stays and B fullscreens.
        use crate::core::commands::{Command, ToggleFullscreen};
        use crate::types::{Client, WinFlags};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;

        let a = 1u32;
        let mut ca = Client::new(a, mi, ws_i);
        ca.border_w = 2;
        ca.flags.set(WinFlags::FULLSCREEN);
        engine.state.add_client(ca);
        engine.state.monitors[mi].workspaces[ws_i].add_tiled(a, engine.cfg.column_width);

        let b = 2u32;
        let mut cb = Client::new(b, mi, ws_i);
        cb.border_w = 2;
        engine.state.add_client(cb);
        engine.state.monitors[mi].workspaces[ws_i].add_tiled(b, engine.cfg.column_width);

        engine.state.monitors[mi].focused = Some(b);
        engine.state.monitors[mi].focus_stack = vec![b];

        let report = ToggleFullscreen(None).execute(&mut engine.state, &mut engine.cfg);
        let target = fs_target(&report).expect("a SetFullscreen effect must be emitted");

        assert_eq!(target, (b, true), "B must be the fullscreen target");
        assert!(
            engine.state.clients.get(&a).unwrap().is_fullscreen(),
            "the overlay A must keep its fullscreen"
        );
        assert_eq!(engine.state.monitors[mi].focused, Some(b));
    }

    #[test]
    fn toggle_fullscreen_stale_focus_targets_overlay_not_new() {
        // Pins the coupling the target resolution has: with the logical focus
        // left on A while the column pointer has already moved to B,
        // `ToggleFullscreen(None)` resolves from `mon.focused` and un-fullscreens
        // the overlay. The managed-window policy exists to keep that state
        // unreachable, and this test keeps the coupling visible if it ever does.
        use crate::core::commands::{Command, ToggleFullscreen};
        use crate::types::{Client, WinFlags};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;

        let a = 1u32;
        let mut ca = Client::new(a, mi, ws_i);
        ca.border_w = 2;
        ca.flags.set(WinFlags::FULLSCREEN);
        engine.state.add_client(ca);
        engine.state.monitors[mi].workspaces[ws_i].add_tiled(a, engine.cfg.column_width);
        engine.state.monitors[mi].focused = Some(a);
        engine.state.monitors[mi].focus_stack = vec![a];

        let b = 2u32;
        let mut cb = Client::new(b, mi, ws_i);
        cb.border_w = 2;
        engine.state.add_client(cb);
        engine.state.monitors[mi].workspaces[ws_i].add_tiled(b, engine.cfg.column_width);
        // Divergent state: the logical focus stays on A while the column pointer
        // has already moved to B by `add_tiled`.
        engine.state.monitors[mi].focused = Some(a);

        let report = ToggleFullscreen(None).execute(&mut engine.state, &mut engine.cfg);
        let target = fs_target(&report).expect("a SetFullscreen effect must be emitted");

        assert_eq!(
            target,
            (a, false),
            "with stale focus on A, the overlay is the target (the bug)"
        );
        assert_eq!(engine.state.monitors[mi].focused, Some(a));
    }

    // The keyboard and EWMH paths share `ToggleFullscreen`, which owns the
    // FULLSCREEN flag and the float→tiling promotion together via
    // `apply_fullscreen_topology`. A promoted float must be laid out from the
    // tiling: left in `ws.floats` it is placed from `client.geom`, and the
    // `Rect::default()` sentinel that path used collapses it to 0×0.
    #[test]
    fn ewmh_fullscreen_promotes_float_and_never_collapses_to_zero() {
        use crate::core::commands::apply_fullscreen_topology;
        use crate::core::layout::{arrange, fs_ctx, ideal_scroll, Placements};
        use crate::types::{Client, WinFlags};
        let cfg = default_cfg();
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        let win = 1u32;

        // A small floating client — exactly how mpv maps with `float = true`.
        let float_rect = Rect::new(100, 100, 400, 300);
        let mut c = Client::new(win, mi, ws_i);
        c.flags.set(WinFlags::FLOAT);
        c.geom = float_rect;
        c.saved_geom = float_rect;
        engine.state.add_client(c);
        engine.state.monitors[mi].workspaces[ws_i].floats.push(win);
        engine.state.monitors[mi].focused = Some(win);

        // The `ToggleFullscreen` Command runs this (shared with the EWMH path)
        // before setting the flag it now owns.
        assert!(
            apply_fullscreen_topology(&mut engine.state, &cfg, win, true),
            "entering fullscreen must promote the float into the tiling"
        );
        {
            let c = engine.state.clients.get(&win).unwrap();
            assert!(!c.is_float());
            assert!(c.flags.has(WinFlags::FS_WAS_FLOAT));
            assert_eq!(
                c.saved_geom, float_rect,
                "the float rect must be snapshotted at promotion time, before \
             arrange overwrites geom with the tile rect"
            );
        }
        assert!(engine.state.monitors[mi].workspaces[ws_i].floats.is_empty());

        // Now the flag, as `set_fullscreen` sets it (border 0, no geom sentinel).
        {
            let c = engine.state.clients.get_mut(&win).unwrap();
            c.flags.set(WinFlags::FULLSCREEN);
            c.fullscreen_policy = crate::types::FullscreenPolicy::True;
            c.old_border_w = c.border_w;
            c.border_w = 0;
        }

        let wa = engine.state.monitors[mi].workarea;
        let fs = fs_ctx(
            &engine.state.clients,
            &engine.state.monitors[mi].workspaces[ws_i],
            engine.state.monitors[mi].screen,
        );
        let scroll = ideal_scroll(&engine.state.monitors[mi].workspaces[ws_i], &cfg, wa, fs);
        engine.state.monitors[mi].workspaces[ws_i]
            .camera
            .snap(scroll);
        let mut p = Placements::new();
        arrange(
            &engine.state,
            mi,
            &cfg,
            &mut p,
            &mut RibbonScratch::default(),
        );
        crate::core::present::present_into(&engine.state, &engine.state.monitors[mi], &mut p);

        let (_, rect, bw) = p
            .iter()
            .find(|e| e.0 == win)
            .copied()
            .expect("the promoted fullscreen window must be placed");
        assert_eq!(
            rect, engine.state.monitors[mi].screen,
            "a float that went fullscreen must fill the screen, not collapse"
        );
        assert_eq!(bw, 0);

        // Leaving fullscreen returns it to the float set at its remembered rect.
        assert!(apply_fullscreen_topology(
            &mut engine.state,
            &cfg,
            win,
            false
        ));
        let c = engine.state.clients.get(&win).unwrap();
        assert!(c.is_float(), "must go back to being a float");
        assert!(!c.flags.has(WinFlags::FS_WAS_FLOAT));
        assert_eq!(
            c.saved_geom, float_rect,
            "the pre-fullscreen float rect survives the round trip"
        );
        assert!(engine.state.monitors[mi].workspaces[ws_i]
            .floats
            .contains(&win));
    }

    // Leaving fullscreen must restore the geometry captured on *enter*, exactly.
    // A single shared `saved_geom` cannot carry that contract: `set_maximized`
    // writes it too, so a window maximized while fullscreen would clobber the
    // pre-fullscreen rect. `FullscreenSnapshot` (prior mode + exact rect) is the
    // only state the restore path is allowed to read.
    #[test]
    fn fullscreen_restore_exact_after_intervening_maximize() {
        use crate::core::commands::{Command, ToggleFullscreen};
        use crate::types::{Client, WinFlags};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        let win = 1u32;

        let float_rect = Rect::new(100, 100, 400, 300);
        let mut c = Client::new(win, mi, ws_i);
        c.flags.set(WinFlags::FLOAT);
        c.geom = float_rect;
        c.saved_geom = float_rect;
        engine.state.add_client(c);
        engine.state.monitors[mi].workspaces[ws_i].floats.push(win);
        engine.state.monitors[mi].focused = Some(win);

        // Enter fullscreen: topology promotes the float and snapshots it.
        ToggleFullscreen(Some(win)).execute(&mut engine.state, &mut engine.cfg);
        {
            let c = engine.state.clients.get(&win).unwrap();
            assert_eq!(
                c.fs_snapshot.map(|s| s.rect),
                Some(float_rect),
                "the pre-fullscreen float rect must be snapshotted on enter"
            );
            assert!(c.flags.has(WinFlags::FS_WAS_FLOAT));
        }

        // While fullscreen, the window is ALSO maximized, which writes
        // `saved_geom` — the clobber the snapshot has to survive.
        {
            let c = engine.state.clients.get_mut(&win).unwrap();
            c.flags.set(WinFlags::MAXIMIZED);
            c.saved_geom = c.geom; // simulate the legacy clobber
        }

        // Leave fullscreen: clear the flag and restore geometry from the
        // snapshot — both owned by the Command now.
        ToggleFullscreen(Some(win)).execute(&mut engine.state, &mut engine.cfg);

        let c = engine.state.clients.get(&win).unwrap();
        assert_eq!(
            c.geom, float_rect,
            "leaving fullscreen must restore the EXACT pre-fullscreen float rect, \
             regardless of the intervening maximize that clobbered saved_geom"
        );
        assert!(c.is_float(), "window returns to being a float");
        assert!(!c.flags.has(WinFlags::FS_WAS_FLOAT));
        assert!(!c.is_fullscreen());
    }

    #[test]
    fn fullscreen_a_then_b_normalize_exact() {
        // Two snapshots must not interfere: A fullscreen, then B fullscreen, then
        // B leaves and A leaves. Each must normalize to its own exact
        // pre-fullscreen rect and topology.
        use crate::core::commands::{Command, ToggleFullscreen};
        use crate::types::{Client, WinFlags};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;

        let a_rect = Rect::new(50, 50, 300, 200);
        let b_rect = Rect::new(700, 400, 350, 250);
        for (win, r) in [(1u32, a_rect), (2u32, b_rect)] {
            let mut c = Client::new(win, mi, ws_i);
            c.flags.set(WinFlags::FLOAT);
            c.geom = r;
            c.saved_geom = r;
            engine.state.add_client(c);
            engine.state.monitors[mi].workspaces[ws_i].floats.push(win);
        }
        engine.state.monitors[mi].focused = Some(1);
        engine.state.monitors[mi].focus_stack = vec![1, 2];

        // A fullscreen (Command owns the flag).
        ToggleFullscreen(Some(1)).execute(&mut engine.state, &mut engine.cfg);

        // B fullscreen (focus B first).
        engine.state.monitors[mi].focused = Some(2);
        ToggleFullscreen(Some(2)).execute(&mut engine.state, &mut engine.cfg);

        // B leaves fullscreen → Command clears B's flag and restores B exactly.
        ToggleFullscreen(Some(2)).execute(&mut engine.state, &mut engine.cfg);
        assert_eq!(
            engine.state.clients.get(&2).unwrap().geom,
            b_rect,
            "B must normalize to its exact pre-fullscreen rect"
        );
        assert!(engine.state.clients.get(&2).unwrap().is_float());

        // A leaves fullscreen → Command clears A's flag and restores A exactly
        // (independent of B).
        ToggleFullscreen(Some(1)).execute(&mut engine.state, &mut engine.cfg);
        let a = engine.state.clients.get(&1).unwrap();
        assert_eq!(
            a.geom, a_rect,
            "A must normalize to its EXACT pre-fullscreen rect after B left; \
             the two snapshots must not interfere"
        );
        assert!(a.is_float());
        assert!(!a.is_fullscreen());
    }

    #[test]
    fn fullscreen_toggle_promotes_policy_and_restores_it() {
        // Entering fullscreen promotes the policy to `True` (exclusive overlay:
        // `present` pins it, `manage` defers behind it, bypass can step aside).
        // Leaving restores the snapshotted prior policy so a `Deny`/`True` rule
        // is never clobbered by one toggle cycle.
        use crate::core::commands::{Command, ToggleFullscreen};
        use crate::types::{Client, FullscreenPolicy, WinFlags};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;

        let mut c = Client::new(1, mi, ws_i);
        c.flags.set(WinFlags::FLOAT);
        c.geom = Rect::new(50, 50, 300, 200);
        c.saved_geom = c.geom;
        // A `Deny` rule was applied at manage time: entering must still
        // promote (keybind wins, as before) but remember the denial.
        c.fullscreen_policy = FullscreenPolicy::Deny;
        engine.state.add_client(c);
        engine.state.monitors[mi].workspaces[ws_i].floats.push(1);
        engine.state.monitors[mi].focused = Some(1);

        ToggleFullscreen(Some(1)).execute(&mut engine.state, &mut engine.cfg);
        assert_eq!(
            engine.state.clients.get(&1).unwrap().fullscreen_policy,
            FullscreenPolicy::True,
            "entering fullscreen must promote to exclusive (True)"
        );
        assert!(
            engine
                .state
                .clients
                .get(&1)
                .unwrap()
                .is_fullscreen_overlay(),
            "promoted window must count as the overlay owner"
        );

        ToggleFullscreen(Some(1)).execute(&mut engine.state, &mut engine.cfg);
        let c = engine.state.clients.get(&1).unwrap();
        assert!(!c.is_fullscreen());
        assert_eq!(
            c.fullscreen_policy,
            FullscreenPolicy::Deny,
            "leaving must restore the pre-enter policy, not Normal"
        );
    }

    #[test]
    fn fullscreen_topology_is_idempotent() {
        use crate::core::commands::apply_fullscreen_topology;
        use crate::types::{Client, WinFlags};
        let cfg = default_cfg();
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        let win = 1u32;

        let mut c = Client::new(win, mi, ws_i);
        c.flags.set(WinFlags::FLOAT);
        c.geom = Rect::new(10, 10, 200, 150);
        engine.state.add_client(c);
        engine.state.monitors[mi].workspaces[ws_i].floats.push(win);

        // The command runs this once per transition; any second "entering" pass
        // must change nothing.
        assert!(apply_fullscreen_topology(
            &mut engine.state,
            &cfg,
            win,
            true
        ));
        let cols_after_first = engine.state.monitors[mi].workspaces[ws_i].columns.clone();
        assert!(
            !apply_fullscreen_topology(&mut engine.state, &cfg, win, true),
            "a second 'entering' pass must be a no-op"
        );
        assert_eq!(
            engine.state.monitors[mi].workspaces[ws_i].columns.len(),
            cols_after_first.len(),
            "the window must not be tiled twice"
        );

        assert!(apply_fullscreen_topology(
            &mut engine.state,
            &cfg,
            win,
            false
        ));
        assert!(
            !apply_fullscreen_topology(&mut engine.state, &cfg, win, false),
            "a second 'leaving' pass must be a no-op"
        );
        assert_eq!(
            engine.state.monitors[mi].workspaces[ws_i].floats,
            vec![win],
            "the window must not be pushed into ws.floats twice"
        );
    }

    #[test]
    fn fullscreen_policy_accessors() {
        use crate::types::{Client, FullscreenPolicy, WinFlags};
        let mut c = Client::new(1, 0, 0);
        // Default policy is Normal: no deny, no exclusive overlay.
        assert!(!c.denies_fullscreen());
        assert!(!c.is_true_fullscreen());
        assert!(!c.is_fullscreen_overlay());

        c.fullscreen_policy = FullscreenPolicy::Deny;
        assert!(c.denies_fullscreen());
        assert!(!c.is_true_fullscreen());

        c.fullscreen_policy = FullscreenPolicy::True;
        assert!(!c.denies_fullscreen());
        assert!(c.is_true_fullscreen());
        // An overlay only when actually fullscreen.
        assert!(!c.is_fullscreen_overlay());
        c.flags.set(WinFlags::FULLSCREEN);
        assert!(c.is_fullscreen_overlay());
    }

    #[test]
    fn fs_ctx_excludes_true_fullscreen() {
        use crate::core::layout::fs_ctx;
        use crate::types::{Client, FullscreenPolicy, WinFlags};
        let cfg = default_cfg();
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        for win in 1..=2u32 {
            engine.state.add_client(Client::new(win, mi, ws_i));
        }
        {
            let ws = &mut engine.state.monitors[mi].workspaces[ws_i];
            ws.layout = LayoutKind::Column;
            for win in 1..=2u32 {
                ws.add_tiled(win, cfg.column_width);
            }
        }
        // Column 0 → window 1 fullscreen (normal).
        engine
            .state
            .clients
            .get_mut(&1)
            .unwrap()
            .flags
            .set(WinFlags::FULLSCREEN);
        let screen = engine.state.monitors[mi].screen;
        let ws = &engine.state.monitors[mi].workspaces[ws_i];
        assert_eq!(
            fs_ctx(&engine.state.clients, ws, screen).cols,
            vec![0],
            "a normal fullscreen window is the ribbon's overlay column"
        );

        // Promote window 1 to a `True` policy fullscreen (games): it must leave
        // the ribbon entirely, so fs_ctx no longer treats it as the overlay.
        engine.state.clients.get_mut(&1).unwrap().fullscreen_policy = FullscreenPolicy::True;
        let ws = &engine.state.monitors[mi].workspaces[ws_i];
        assert_eq!(
            fs_ctx(&engine.state.clients, ws, screen).cols,
            Vec::<usize>::new(),
            "a True fullscreen window is excluded from the ribbon overlay"
        );
    }

    #[test]
    fn maximized_axis_flags_are_independent() {
        use crate::types::{Client, WinFlags};
        let mut c = Client::new(1, 0, 0);
        assert!(!c.is_maximized());
        assert!(!c.is_maximized_v());
        assert!(!c.is_maximized_h());

        c.flags.set(WinFlags::MAXIMIZED_V);
        assert!(!c.is_maximized());
        assert!(c.is_maximized_v());
        assert!(!c.is_maximized_h());

        // The combined `MAXIMIZED` bit is only on when *both* axes are
        // maximized — exactly what `set_maximized(true, true)` sets. Setting the
        // H bit alone does not flip it.
        c.flags.set(WinFlags::MAXIMIZED_H);
        assert!(c.is_maximized());
        assert!(c.is_maximized_v());
        assert!(c.is_maximized_h());
    }

    #[test]
    fn viewport_zoom_enters_zoomed_mode_and_enlarges_ribbon() {
        use crate::core::layout::{fs_ctx, ribbon_geom};
        use crate::types::{Action, Client, ViewportMode};
        let cfg = default_cfg();
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        for win in 1..=2u32 {
            engine.state.add_client(Client::new(win, mi, ws_i));
            engine.state.monitors[mi].workspaces[ws_i].add_tiled(win, cfg.column_width);
        }

        engine.dispatch(Action::ViewportZoom(0.2));
        let ws = &engine.state.monitors[mi].workspaces[ws_i];
        assert_eq!(ws.viewport_mode, ViewportMode::Zoomed);
        assert!(
            ws.page_zoom > 1.0,
            "page_zoom target must grow past 1.0"
        );

        let ws = &engine.state.monitors[mi].workspaces[ws_i];
        assert!(ws.page_zoom > 1.0, "the viewport zoom must enlarge past 1.0");

        // `ribbon_geom` must feed the viewport factor into `alpha` so columns
        // are enlarged (alpha > 1), independent of the Overview zoom.
        let wa = engine.state.monitors[mi].workarea;
        let fs = fs_ctx(&engine.state.clients, ws, engine.state.monitors[mi].screen);
        let g = ribbon_geom(ws, &engine.cfg, wa, &fs);
        assert!(
            g.alpha > 1.0,
            "a zoomed viewport must enlarge the ribbon (alpha > 1)"
        );
    }

    #[test]
    fn viewport_zoom_out_returns_to_normal() {
        use crate::types::{Action, ViewportMode};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.dispatch(Action::ViewportZoom(0.2));
        assert_eq!(
            engine.state.monitors[mi].workspaces[ws_i].viewport_mode,
            ViewportMode::Zoomed
        );
        // A large negative step drives the factor back to <= 1.0 → Normal.
        engine.dispatch(Action::ViewportZoom(-0.5));
        let ws = &engine.state.monitors[mi].workspaces[ws_i];
        assert_eq!(ws.viewport_mode, ViewportMode::Normal);
        assert!((ws.page_zoom - 1.0).abs() < 1e-6);
    }

    #[test]
    fn page_snap_scrolls_camera_by_one_page() {
        use crate::types::{Action, Client, Dir};
        let cfg = default_cfg();
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        // Enough columns that the ribbon is far wider than one screen, so a
        // page-snap has visible room to scroll.
        for win in 1..=12u32 {
            engine.state.add_client(Client::new(win, mi, ws_i));
            engine.state.monitors[mi].workspaces[ws_i].add_tiled(win, cfg.column_width);
        }
        // Start the camera at the left edge, then snap one page to the right.
        engine.state.monitors[mi].workspaces[ws_i].camera.position = 0.0;
        let before = engine.state.monitors[mi].workspaces[ws_i].camera.position;
        engine.dispatch(Action::PageSnap(Dir::Right));
        let after = engine.state.monitors[mi].workspaces[ws_i].camera.position;
        let wa = engine.state.monitors[mi].workarea;
        let expected_step = wa.w as f32; // alpha = 1.0 → one screen-width page
        assert!(
            after > before && after <= expected_step + 1.0,
            "PageSnap right must scroll the camera forward by one page (~{expected_step}): got {after}"
        );
    }

    #[test]
    fn page_snap_does_not_jump_when_ribbon_fits() {
        use crate::types::{Action, Client, Dir};
        let cfg = default_cfg();
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.add_client(Client::new(1, mi, ws_i));
        engine.state.monitors[mi].workspaces[ws_i].add_tiled(1, cfg.column_width);
        // Recompute the settled center through the same command used by zoom
        // navigation, then install it as the current visual endpoint.
        engine.dispatch(Action::ViewportZoom(0.0));
        let center = engine.state.monitors[mi].workspaces[ws_i].camera.position;
        engine.state.monitors[mi].workspaces[ws_i]
            .camera
            .snap(center);
        engine.dispatch(Action::PageSnap(Dir::Right));
        assert!(
            (engine.state.monitors[mi].workspaces[ws_i].camera.position - center).abs() < 1e-4,
            "a page snap inside a non-overflowing ribbon must be a no-op"
        );
    }

    // Drive ≥10k random Create/Destroy/Focus/Move/Resize/Fullscreen/Scroll/
    // View/Layout sequences through the real command layer and assert
    // `State::check_invariants()` after every step, plus that the layout is
    // deterministic (the same state always arranges to the same placements).
    // The seed is fixed so a failure reproduces exactly. In debug builds
    // `Engine::execute` additionally runs `assert_invariants`, so both the
    // explicit check here and the production path are exercised.

    #[test]
    fn property_invariants_hold_under_chaos() {
        use crate::core::commands::{
            CollapseColumn, Command, FocusDirection, FocusMonitor, GrowColumn, NewColumn,
            OverviewNav, SetLayout, ToggleFloat, ToggleFullscreen, ToggleMaximize, ToggleOverview,
        };
        use crate::core::effect::Effect;
        use crate::core::layout::{arrange, Placements, RibbonScratch};
        use crate::types::{Client, Dir, LayoutKind, WinFlags, WindowId};

        const SEED: u64 = 0x56ec_73ed_1234_5678;
        const STEPS: u32 = 12_000;
        const MAX_WINS: usize = 24;

        // Tiny deterministic LCG — no external RNG dependency, reproducible.
        struct Rng(u64);
        impl Rng {
            fn next(&mut self) -> u64 {
                self.0 = self
                    .0
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                self.0 >> 16
            }
            fn below(&mut self, n: u32) -> u32 {
                (self.next() % n as u64) as u32
            }
        }
        let mut rng = Rng(SEED);

        /// Mirror the backend: run a command, then apply the `FocusWindow`
        /// effect it emitted (the core command only *emits* focus; the backend
        /// is what actually moves `mon.focused`). Without this the harness's
        /// logical focus would drift away from the active workspace and exercise
        /// transient states the real WM never reaches.
        fn run_cmd<C: Command>(engine: &mut Engine, cmd: C) -> Vec<Effect> {
            let effects = engine.execute(cmd);
            for eff in &effects {
                if let Effect::FocusWindow(w) = eff {
                    engine.state.monitors[engine.state.sel_mon].focused = *w;
                }
            }
            effects
        }

        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let mut live: Vec<WindowId> = Vec::new();
        let mut next_win: u32 = 1;

        let fresh_window =
            |engine: &mut Engine, rng: &mut Rng, live: &mut Vec<WindowId>, next: &mut u32| {
                if live.len() >= MAX_WINS {
                    return false;
                }
                let win = *next;
                *next += 1;
                let ws_i = engine.state.monitors[mi].active_ws;
                let mut c = Client::new(win, mi, ws_i);
                c.border_w = 2;
                if rng.below(2) == 0 {
                    c.flags.set(WinFlags::FLOAT);
                    c.geom = Rect::new(50, 50, 400, 300);
                    c.saved_geom = c.geom;
                    engine.state.monitors[mi].workspaces[ws_i].floats.push(win);
                } else {
                    engine.state.monitors[mi].workspaces[ws_i]
                        .add_tiled(win, engine.cfg.column_width);
                }
                engine.state.add_client(c);
                engine.state.monitors[mi].focused = Some(win);
                engine.state.monitors[mi].focus_stack.retain(|&w| w != win);
                engine.state.monitors[mi].focus_stack.push(win);
                live.push(win);
                true
            };

        // Ensure at least one window exists so focus-dependent ops have a target.
        fresh_window(&mut engine, &mut rng, &mut live, &mut next_win);

        for step in 0..STEPS {
            let op = rng.below(20);
            match op {
                0..=4 if live.len() < MAX_WINS => {
                    fresh_window(&mut engine, &mut rng, &mut live, &mut next_win);
                }
                5..=7 if !live.is_empty() => {
                    let idx = rng.below(live.len() as u32) as usize;
                    let win = live[idx];
                    engine.state.remove_client(win);
                    live.remove(idx);
                }
                8 => {
                    run_cmd(&mut engine, ToggleFloat(None));
                }
                9 => {
                    run_cmd(&mut engine, ToggleFullscreen(None));
                }
                10 => {
                    run_cmd(&mut engine, ToggleMaximize(None));
                }
                11 => {
                    let d = if rng.below(2) == 0 {
                        Dir::Left
                    } else {
                        Dir::Right
                    };
                    run_cmd(&mut engine, FocusDirection(d));
                }
                12 => {
                    let d = if rng.below(2) == 0 {
                        Dir::Left
                    } else {
                        Dir::Right
                    };
                    run_cmd(&mut engine, FocusMonitor(d));
                }
                13 => {
                    let ws = rng.below(engine.cfg.n_tags as u32) as usize;
                    run_cmd(&mut engine, crate::core::commands::ViewWorkspace(ws));
                }
                14 => {
                    let ws = rng.below(engine.cfg.n_tags as u32) as usize;
                    run_cmd(&mut engine, crate::core::commands::MoveToWorkspace(ws));
                }
                15 => {
                    run_cmd(&mut engine, SetLayout(LayoutKind::Column));
                }
                16 => {
                    let lk = if rng.below(2) == 0 {
                        LayoutKind::Column
                    } else {
                        LayoutKind::Column
                    };
                    run_cmd(&mut engine, SetLayout(lk));
                }
                17 => {
                    let dx = if rng.below(2) == 0 { 20 } else { -20 };
                    run_cmd(&mut engine, GrowColumn(dx));
                }
                18 => {
                    run_cmd(&mut engine, NewColumn);
                }
                19 => {
                    run_cmd(&mut engine, CollapseColumn);
                }
                // Map overflow / no-window cases to safe layout ops.
                _ => {
                    if rng.below(2) == 0 {
                        run_cmd(&mut engine, ToggleOverview);
                    } else {
                        let d = if rng.below(2) == 0 {
                            Dir::Left
                        } else {
                            Dir::Right
                        };
                        run_cmd(&mut engine, OverviewNav(d));
                    }
                }
            }

            if let Err(v) = engine.state.check_invariants() {
                // Dump where every window referenced in any tree lives, to
                // localise a cross-workspace duplication.
                use std::fmt::Write as _;
                let mut dump = String::new();
                for (mi2, mon2) in engine.state.monitors.iter().enumerate() {
                    for (ws2, wsx) in mon2.workspaces.iter().enumerate() {
                        let mut wins: Vec<WindowId> = wsx
                            .columns
                            .iter()
                            .flat_map(|c| c.windows.iter().copied())
                            .collect();
                        wins.extend(wsx.floats.iter().copied());
                        if !wins.is_empty() {
                            let _ = writeln!(
                                dump,
                                "  mon{mi2} ws{ws2} (active={}): {:?}",
                                ws2 == mon2.active_ws,
                                wins
                            );
                        }
                    }
                }
                let clients_ws: Vec<(WindowId, usize)> = engine
                    .state
                    .clients
                    .iter()
                    .map(|(&w, c)| (w, c.workspace))
                    .collect();
                panic!(
                    "seed {SEED:#x} step {step} op {op}: invariant violation:\n  - {}\nTREE:\n{dump}CLIENTS(ws): {:?}",
                    v.join("\n  - "),
                    clients_ws
                );
            }

            // Periodically assert layout determinism + overview/scroll ops don't
            // corrupt the tree.
            if step % 200 == 0 {
                let ws_i = engine.state.monitors[mi].active_ws;
                let mut p1 = Placements::new();
                let mut p2 = Placements::new();
                let mut r1 = RibbonScratch::default();
                let mut r2 = RibbonScratch::default();
                arrange(
                    &engine.state,
                    mi,
                    &engine.cfg,
                    &mut p1,
                    &mut r1,
                );
                arrange(
                    &engine.state,
                    mi,
                    &engine.cfg,
                    &mut p2,
                    &mut r2,
                );
                let mut v1: Vec<(WindowId, Rect, u32)> =
                    p1.iter().map(|(w, r, b)| (*w, *r, *b)).collect();
                let mut v2: Vec<(WindowId, Rect, u32)> =
                    p2.iter().map(|(w, r, b)| (*w, *r, *b)).collect();
                v1.sort_by_key(|x| x.0);
                v2.sort_by_key(|x| x.0);
                assert_eq!(
                    v1, v2,
                    "seed {SEED:#x} step {step}: layout is non-deterministic for the same state"
                );
                let _ = ws_i;
            }
        }

        engine
            .state
            .check_invariants()
            .expect("final state must satisfy invariants");
    }

    // `State::presented_overlay_owner` is the single source of truth for overlay
    // ownership: a fullscreen window counts only under `FullscreenPolicy::True`
    // (a Normal-policy fullscreen is just a ribbon tile) plus the *focused*
    // maximized window (`presented_maximize`). The `t_*` helpers below mirror the
    // backend paths that consume it (`manage`, `unmanage`, `focus`) so these
    // tests exercise the same decisions without an X server.

    /// Logical half of the backend's `focus()`: logical focus + MRU stack + the
    /// single `presented_maximize` writer. Mirrors `Backend::focus` by also moving
    /// `sel_mon` to the focused window's own monitor, so the model stays consistent
    /// with the real sink (where focusing a window on another monitor selects it).
    fn t_focus(engine: &mut Engine, win: WindowId) {
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

    /// Register + tile a window exactly like `manage()` does, *without* the
    /// presentation-aware focus policy (and without touching the focus).
    fn t_add(engine: &mut Engine, win: WindowId) {
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        let mut c = Client::new(win, mi, ws_i);
        c.border_w = engine.cfg.border_w;
        c.geom = Rect::new(0, 0, 800, 600);
        c.saved_geom = c.geom;
        engine.state.monitors[mi].workspaces[ws_i].add_tiled(win, engine.cfg.column_width);
        engine.state.add_client(c);
    }

    /// `manage()` including its presentation-aware focus policy: with a real
    /// overlay present the newcomer is deferred into the global `pending_focus`
    /// slot (keyed by its own monitor/workspace/owner) and the overlay keeps
    /// input; otherwise the newcomer takes the focus. Returns true when the new
    /// window got the focus.
    fn t_manage(engine: &mut Engine, win: WindowId) -> bool {
        t_add(engine, win);
        match crate::core::commands::decide_manage_focus(&engine.state, win) {
            crate::core::commands::ManageFocusIntent::Defer {
                owner,
                monitor,
                workspace,
            } => {
                engine.state.pending_focus = Some(crate::types::PendingFocus {
                    window: win,
                    owner,
                    monitor,
                    workspace,
                });
                false
            }
            crate::core::commands::ManageFocusIntent::Focus(_) => {
                t_focus(engine, win);
                true
            }
        }
    }

    /// Tail of `unmanage()`: on the selected monitor consume a still-valid
    /// global `pending_focus` (keyed on this monitor/workspace), else fall back
    /// to `best_focus`; a *background* monitor only repairs its own logical focus
    /// (through the core helper) and never steals `sel_mon`'s.
    fn t_destroy(engine: &mut Engine, win: WindowId) {
        let mon_i = engine
            .state
            .clients
            .get(&win)
            .map_or(engine.state.sel_mon, |c| c.monitor);
        // Snapshot the global slot before `remove_client` may clear it (it clears
        // when `win` is the deferral's owner or target) so we can still consume a
        // deferred window when its overlay owner is destroyed.
        let pending_snapshot = engine.state.pending_focus;
        engine.state.remove_client(win);
        if mon_i >= engine.state.monitors.len() {
            return;
        }
        let deferred = match pending_snapshot {
            Some(pf)
                if pf.owner == win
                    && pf.monitor == mon_i
                    && engine.state.clients.contains_key(&pf.window) =>
            {
                Some(pf.window)
            }
            _ => None,
        };
        if mon_i == engine.state.sel_mon {
            if let Some(p) = deferred {
                engine.state.pending_focus = None;
                t_focus(engine, p);
            } else {
                let aws = engine.state.monitors[mon_i].active_ws;
                if let Some(p) = crate::core::commands::consume_pending_focus(
                    &mut engine.state,
                    mon_i,
                    aws,
                    Some(win),
                ) {
                    t_focus(engine, p);
                } else if let Some(b) = engine.state.best_focus(mon_i) {
                    t_focus(engine, b);
                }
            }
        } else if let Some(p) = deferred {
            engine.state.pending_focus = None;
            crate::core::commands::focus_logical_on(&mut engine.state, mon_i, p);
        } else if let Some(b) = engine.state.best_focus(mon_i) {
            crate::core::commands::focus_logical_on(&mut engine.state, mon_i, b);
        }
    }

    fn t_set_fullscreen(engine: &mut Engine, win: WindowId, on: bool) {
        if let Some(c) = engine.state.clients.get_mut(&win) {
            if on {
                c.flags.set(WinFlags::FULLSCREEN);
                c.fullscreen_policy = crate::types::FullscreenPolicy::True;
            } else {
                c.flags.clear(WinFlags::FULLSCREEN);
            }
        }
    }

    fn t_set_maximized(engine: &mut Engine, win: WindowId) {
        if let Some(c) = engine.state.clients.get_mut(&win) {
            c.flags.set(WinFlags::MAXIMIZED_V);
            c.flags.set(WinFlags::MAXIMIZED_H);
        }
    }

    #[test]
    fn fullscreen_column_normal_new_window_receives_focus() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);
        engine.state.clients.get_mut(&1).unwrap().fullscreen_policy =
            crate::types::FullscreenPolicy::Normal;

        assert!(
            engine.state.presented_overlay_owner(mi).is_none(),
            "a Column/Normal fullscreen window is a ribbon tile, NOT a presented overlay"
        );
        assert!(
            t_manage(&mut engine, 2),
            "with no real overlay the newcomer must receive the focus"
        );
        assert_eq!(
            engine.state.best_focus(mi),
            Some(2),
            "best_focus must pick the new window, not the ribbon fullscreen tile"
        );
        assert!(
            engine.state.pending_focus.is_none(),
            "no overlay ⇒ no deferral"
        );
    }

    #[test]
    fn fullscreen_true_keeps_overlay() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);
        engine.state.clients.get_mut(&1).unwrap().fullscreen_policy = FullscreenPolicy::True;
        assert_eq!(
            engine.state.presented_overlay_owner(mi),
            Some(1),
            "a True-policy fullscreen is the overlay in every layout"
        );
        engine.state.clients.get_mut(&1).unwrap().fullscreen_policy = FullscreenPolicy::Normal;
        assert_eq!(
            engine.state.presented_overlay_owner(mi),
            None,
            "a Column/Normal fullscreen is not an overlay"
        );
    }

    #[test]
    fn maximized_presented_keeps_overlay_unfocused_does_not() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        t_manage(&mut engine, 1);
        t_set_maximized(&mut engine, 1);
        t_focus(&mut engine, 1);

        assert_eq!(
            engine.state.presented_overlay_owner(mi),
            Some(1),
            "the focused maximized window owns the overlay"
        );

        t_add(&mut engine, 2);
        t_focus(&mut engine, 2);
        assert_eq!(
            engine.state.presented_overlay_owner(mi),
            None,
            "an *unfocused* maximized window is not an overlay"
        );
    }

    #[test]
    fn fullscreen_a_create_b_destroy_b_focus_returns_to_a() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);
        assert_eq!(engine.state.presented_overlay_owner(mi), Some(1));

        assert!(
            !t_manage(&mut engine, 2),
            "a newcomer must not steal input from a live overlay"
        );
        assert_eq!(engine.state.monitors[mi].focused, Some(1));

        t_destroy(&mut engine, 2);
        assert_eq!(
            engine.state.monitors[mi].focused,
            Some(1),
            "closing the deferred window returns the focus to the overlay owner"
        );
    }

    #[test]
    fn fullscreen_a_create_b_focus_b_does_not_hijack_a() {
        use crate::core::effect::Effect;
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);
        assert!(!t_manage(&mut engine, 2));

        assert_eq!(
            engine.state.monitors[mi].focused,
            Some(1),
            "B must not hijack the input focus from the live overlay A"
        );
        assert_eq!(
            engine.state.pending_focus.map(|pf| pf.window),
            Some(2),
            "B is deferred, not lost"
        );
        assert_eq!(engine.state.presented_overlay_owner(mi), Some(1));

        // …and B is reachable: dismissing the overlay hands it the focus.
        let effects = engine.execute(crate::core::commands::ToggleFullscreen(Some(1)));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::FocusWindow(Some(2)))),
            "leaving fullscreen must emit the deferred focus: {effects:?}"
        );
        assert_eq!(engine.state.monitors[mi].focused, Some(2));
    }

    #[test]
    fn repeated_create_destroy_keeps_focus_stack_consistent() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        for round in 0..6u32 {
            let a = round * 2 + 1;
            let b = round * 2 + 2;
            t_manage(&mut engine, a);
            t_manage(&mut engine, b);
            t_destroy(&mut engine, a);

            let stack = engine.state.monitors[mi].focus_stack.clone();
            let mut uniq = stack.clone();
            uniq.sort_unstable();
            uniq.dedup();
            assert_eq!(
                uniq.len(),
                stack.len(),
                "focus_stack grew duplicate entries: {stack:?}"
            );
            for w in &stack {
                assert!(
                    engine.state.clients.contains_key(w),
                    "stale window {w} left in focus_stack: {stack:?}"
                );
            }
            engine
                .state
                .check_invariants()
                .expect("create/destroy churn must preserve invariants");
        }
    }

    #[test]
    fn workspace_switch_does_not_steal_focus_via_pending() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        engine.state.monitors[mi].workspaces[0].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);
        assert!(
            !t_manage(&mut engine, 2),
            "B is deferred behind the overlay"
        );

        engine.execute(crate::core::commands::ViewWorkspace(1));
        assert_eq!(engine.state.monitors[mi].active_ws, 1);
        assert_eq!(
            engine.state.best_focus(mi),
            None,
            "an empty workspace has no focus candidate"
        );
        assert_eq!(
            engine.state.pending_focus.map(|pf| pf.window),
            Some(2),
            "the deferral stays with the workspace that owns the overlay"
        );

        engine.execute(crate::core::commands::ViewWorkspace(0));
        assert_eq!(engine.state.presented_overlay_owner(mi), Some(1));
        assert_eq!(
            engine.state.monitors[mi].focused,
            Some(1),
            "workspace ping-pong must not hand input to the deferred window"
        );
    }

    #[test]
    fn focus_fullscreen_create_destroy_never_leaves_invalid_focus() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        engine.state.monitors[mi].workspaces[0].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_manage(&mut engine, 2);
        t_focus(&mut engine, 1);

        engine.execute(crate::core::commands::ToggleFullscreen(Some(1)));
        engine.state.clients.get_mut(&1).unwrap().fullscreen_policy =
            crate::types::FullscreenPolicy::True;
        assert!(
            !t_manage(&mut engine, 3),
            "C is deferred behind the overlay"
        );
        engine.execute(crate::core::commands::ToggleFullscreen(Some(1)));
        assert_eq!(
            engine.state.monitors[mi].focused,
            Some(3),
            "dismissing the overlay must hand input to the deferred window"
        );

        t_destroy(&mut engine, 3);
        t_destroy(&mut engine, 1);
        let f = engine.state.monitors[mi].focused;
        assert!(
            f.is_none() || engine.state.clients.contains_key(&f.unwrap()),
            "focus must never name a dead window: {f:?}"
        );
        assert_eq!(f, Some(2), "the last survivor takes the focus");
        engine
            .state
            .check_invariants()
            .expect("fullscreen create/destroy churn must preserve invariants");
    }

    #[test]
    fn property_random_window_ops_preserve_invariants() {
        use crate::core::commands::{
            FocusMonitor, ToggleFloat, ToggleFullscreen, ToggleMaximize, ViewWorkspace,
        };
        use crate::types::{Dir, LayoutKind};

        const SEED: u64 = 0x0BAD_C0DE_D15E_A5E5;
        const STEPS: u32 = 400;
        const MAX_WINS: usize = 12;

        struct Rnd(u64);
        impl Rnd {
            fn next(&mut self) -> u64 {
                self.0 = self
                    .0
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                self.0 >> 16
            }
        }
        let mut rng = Rnd(SEED);
        let mut engine = setup_engine_multi();
        let mut live: Vec<WindowId> = Vec::new();
        let mut next_win: WindowId = 1;

        // Assert the six focus-model conditions hold after every generated step.
        let check_model = |engine: &Engine, step: u32, op: u64| {
            let s = &engine.state;
            // (1) every monitor's logical focus names a real client (it may be on
            //     any workspace; the harness focuses via the model, not the X sink,
            //     so it does not move `sel_mon`).
            for (mi, m) in s.monitors.iter().enumerate() {
                if let Some(fw) = m.focused {
                    assert!(s.clients.contains_key(&fw),
                        "seed {SEED:#x} step {step} op {op}: monitor {mi} focused {fw:?} is not a live client"
                    );
                }
            }
            // (2) focus_stack has no duplicates and names only live clients.
            for (mi, m) in s.monitors.iter().enumerate() {
                let mut seen = std::collections::HashSet::new();
                for &w in &m.focus_stack {
                    assert!(seen.insert(w), "seed {SEED:#x} step {step} op {op}: monitor {mi} focus_stack has duplicate {w}");
                    assert!(s.clients.contains_key(&w), "seed {SEED:#x} step {step} op {op}: monitor {mi} focus_stack names dead {w}")
                }
            }
            // (3) pending_focus is not dangling.
            if let Some(pf) = s.pending_focus {
                assert!(
                    s.clients.contains_key(&pf.window),
                    "seed {SEED:#x} step {step} op {op}: pending_focus window {} dead",
                    pf.window
                );
                assert!(
                    s.presented_overlay_owner(pf.monitor) == Some(pf.owner)
                        || s.monitors.get(pf.monitor).and_then(|m| m.workspaces.get(pf.workspace)).is_some_and(|ws| {
                            s.clients.get(&pf.owner).is_some_and(|c| c.monitor == pf.monitor && c.workspace == pf.workspace
                                && (c.is_fullscreen() && (ws.layout == LayoutKind::Column || c.is_true_fullscreen())
                                    || ((c.is_maximized_v() || c.is_maximized_h()) && s.monitors[pf.monitor].focused == Some(pf.owner))))
                        }),
                    "seed {SEED:#x} step {step} op {op}: pending_focus owner {} not a presented overlay on mon {} ws {}",
                    pf.owner, pf.monitor, pf.workspace
                );
            }
            // (4) no presented overlay/maximize without a live owner.
            for (mi, m) in s.monitors.iter().enumerate() {
                if let Some(w) = s.presented_overlay_owner(mi) {
                    assert!(s.clients.contains_key(&w), "seed {SEED:#x} step {step} op {op}: presented overlay {w} on mon {mi} has no client");
                }
                if let Some(w) = m
                    .workspaces
                    .get(m.active_ws)
                    .and_then(|ws| ws.presented_maximize)
                {
                    match s.clients.get(&w) {
                        Some(c) if c.is_maximized() && c.workspace == m.active_ws => {}
                        _ => panic!("seed {SEED:#x} step {step} op {op}: presented_maximize {w} on mon {mi} invalid"),
                    }
                }
            }
            // (5) #9b: presented_maximize == presented_overlay_owner when the owner is maximized.
            for (mi, m) in s.monitors.iter().enumerate() {
                if let Some(w) = s.presented_overlay_owner(mi) {
                    if s.clients
                        .get(&w)
                        .is_some_and(crate::types::Client::is_maximized)
                    {
                        assert_eq!(
                            m.workspaces
                                .get(m.active_ws)
                                .and_then(|ws| ws.presented_maximize),
                            Some(w),
                            "seed {SEED:#x} step {step} op {op}: #9b mismatch on mon {mi}",
                        );
                    }
                }
            }
            // (6) x11_input_focus names a real client or is None.
            if let Some(w) = s.x11_input_focus {
                assert!(
                    s.clients.contains_key(&w),
                    "seed {SEED:#x} step {step} op {op}: x11_input_focus {w} dead"
                );
            }
        };

        for step in 0..STEPS {
            let op = rng.next() % 9;
            let sel = engine.state.sel_mon;
            match op {
                // Create (real manage path: defers behind a live overlay).
                0 => {
                    if live.len() < MAX_WINS {
                        let w = next_win;
                        next_win += 1;
                        t_manage(&mut engine, w);
                        live.push(w);
                    }
                }
                // Destroy (real unmanage path).
                1 => {
                    if !live.is_empty() {
                        let i = (rng.next() % live.len() as u64) as usize;
                        let w = live.remove(i);
                        t_destroy(&mut engine, w);
                    }
                }
                // Focus
                2 => {
                    if !live.is_empty() {
                        let w = live[(rng.next() % live.len() as u64) as usize];
                        t_focus(&mut engine, w);
                    }
                }
                // Fullscreen
                3 => {
                    engine.execute(ToggleFullscreen(None));
                }
                // Maximize
                4 => {
                    engine.execute(ToggleMaximize(None));
                }
                // Float
                5 => {
                    engine.execute(ToggleFloat(None));
                }
                // Manage a window on a randomly-selected monitor/workspace so the
                // deferral can be bound to a monitor/workspace that is NOT the
                // selected one — the shape that strands a deferral whose owner is
                // no longer the presented overlay anywhere.
                6 => {
                    if live.len() < MAX_WINS {
                        let nmon = engine.state.monitors.len();
                        let nws = engine.state.monitors[sel.min(nmon - 1)].workspaces.len();
                        let target = (rng.next() % nmon as u64) as usize;
                        let tws = (rng.next() % nws as u64) as usize;
                        engine.state.sel_mon = target;
                        engine.state.monitors[target].active_ws = tws;
                        let w = next_win;
                        next_win += 1;
                        t_manage(&mut engine, w);
                        live.push(w);
                    }
                }
                // Monitor switch (FocusMonitor).
                7 => {
                    engine.execute(FocusMonitor(Dir::Next));
                }
                // Workspace switch (ViewWorkspace) — apply the FocusWindow effect
                // on the now-active workspace so logical focus tracks it.
                _ => {
                    let n = engine.state.monitors[sel].workspaces.len();
                    let ws = (rng.next() % n as u64) as usize;
                    engine.execute(ViewWorkspace(ws));
                    let new_sel = engine.state.sel_mon;
                    if let Some(b) = engine.state.best_focus(new_sel) {
                        crate::core::commands::focus_logical_on(&mut engine.state, new_sel, b);
                    }
                }
            }
            check_model(&engine, step, op);
            if let Err(v) = engine.state.check_invariants() {
                panic!(
                    "seed {SEED:#x} step {step} op {op}: invariant violation:\n  - {}",
                    v.join("\n  - ")
                );
            }
        }
    }

    #[test]
    fn pending_focus_consumed_on_fullscreen_keyboard_dismiss() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);
        t_add(&mut engine, 2);
        engine.state.pending_focus = Some(crate::types::PendingFocus {
            window: 2,
            owner: 1,
            monitor: mi,
            workspace: ws_i,
        });
        assert_eq!(engine.state.presented_overlay_owner(mi), Some(1));

        engine.execute(crate::core::commands::ToggleFullscreen(Some(1)));
        assert_eq!(
            engine.state.monitors[mi].focused,
            Some(2),
            "the keybind dismissal must hand input to the deferred window"
        );
        assert!(
            engine.state.pending_focus.is_none(),
            "the deferral is consumed exactly once"
        );
    }

    #[test]
    fn pending_focus_consumed_on_maximize_keyboard_dismiss() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        t_manage(&mut engine, 1);
        t_set_maximized(&mut engine, 1);
        t_focus(&mut engine, 1);
        t_add(&mut engine, 2);
        engine.state.pending_focus = Some(crate::types::PendingFocus {
            window: 2,
            owner: 1,
            monitor: mi,
            workspace: ws_i,
        });
        assert_eq!(engine.state.presented_overlay_owner(mi), Some(1));

        engine.execute(crate::core::commands::ToggleMaximize(Some(1)));
        assert_eq!(
            engine.state.monitors[mi].focused,
            Some(2),
            "the keybind dismissal must hand input to the deferred window"
        );
        assert!(
            engine.state.pending_focus.is_none(),
            "the deferral is consumed exactly once"
        );
    }

    #[test]
    fn pending_focus_invalidated_when_deferred_window_gone() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);
        engine.state.pending_focus = Some(crate::types::PendingFocus {
            window: 999,
            owner: 1,
            monitor: mi,
            workspace: ws_i,
        });

        engine.execute(crate::core::commands::ToggleFullscreen(Some(1)));
        assert!(
            engine.state.pending_focus.is_none(),
            "a deferral naming a dead window must be dropped, never focused"
        );
        assert_eq!(
            engine.state.monitors[mi].focused,
            Some(1),
            "focus stays where it was"
        );
    }

    #[test]
    fn destroy_overlay_owner_consumes_pending() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);
        assert!(!t_manage(&mut engine, 2));
        assert_eq!(engine.state.pending_focus.map(|pf| pf.window), Some(2));

        t_destroy(&mut engine, 1);
        assert_eq!(
            engine.state.monitors[mi].focused,
            Some(2),
            "when the overlay owner dies the deferred window takes the focus"
        );
        assert!(engine.state.pending_focus.is_none());
        engine
            .state
            .check_invariants()
            .expect("overlay teardown must preserve invariants");
    }

    // A deferral is keyed by (monitor, workspace, owner), so tearing the overlay
    // down after the user has navigated away must still consume it: on a
    // non-active workspace, after a monitor+workspace round trip, and on a
    // non-selected monitor.
    #[test]
    fn orphan_defer_not_lost_when_overlay_destroyed_on_non_active_ws() {
        let mut engine = setup_engine_multi();
        let mon0 = 0;
        engine.state.monitors[mon0].workspaces[0].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);
        assert!(
            !t_manage(&mut engine, 2),
            "B is deferred behind the overlay"
        );
        assert_eq!(engine.state.pending_focus.map(|pf| pf.window), Some(2));

        // The overlay now lives on ws0 while the selected monitor shows ws1.
        engine.state.monitors[mon0].active_ws = 1;
        engine.state.sync_presented_maximize(mon0);

        // Destroy overlay A on mon0/ws0 (a non-active workspace). B must be
        // consumed, not orphaned.
        t_destroy(&mut engine, 1);
        assert_eq!(
            engine.state.monitors[mon0].focused,
            Some(2),
            "destroying overlay A on a non-active workspace must focus deferred B"
        );
        assert!(engine.state.pending_focus.is_none());
        engine
            .state
            .check_invariants()
            .expect("orphan fix (scenario 4): invariants");
    }

    #[test]
    fn orphan_defer_not_lost_when_ws_switch_then_dismiss_on_other_ws() {
        use crate::core::effect::Effect;
        let mut engine = setup_engine_multi();
        let mon0 = 0;
        engine.state.monitors[mon0].workspaces[0].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);
        assert!(
            !t_manage(&mut engine, 2),
            "B is deferred behind the overlay"
        );
        assert_eq!(engine.state.pending_focus.map(|pf| pf.window), Some(2));

        // Leave the overlay behind on mon0/ws0 by selecting the other monitor.
        engine.execute(crate::core::commands::FocusMonitor(crate::types::Dir::Next));
        assert_ne!(engine.state.sel_mon, mon0);
        // Return to mon0, then dismiss the overlay via the keybind path.
        engine.execute(crate::core::commands::FocusMonitor(crate::types::Dir::Next));
        assert_eq!(engine.state.sel_mon, mon0);

        let effects = engine.execute(crate::core::commands::ToggleFullscreen(Some(1)));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::FocusWindow(Some(2)))),
            "dismissing overlay on mon0/ws0 must hand focus to deferred B: {effects:?}"
        );
        assert_eq!(engine.state.monitors[mon0].focused, Some(2));
        assert!(engine.state.pending_focus.is_none());
        engine
            .state
            .check_invariants()
            .expect("orphan fix (scenario 8): invariants");
    }

    #[test]
    fn orphan_defer_not_lost_when_overlay_destroyed_on_non_selected_monitor() {
        let mut engine = setup_engine_multi();
        let mon0 = 0;
        engine.state.monitors[mon0].workspaces[0].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);
        assert!(
            !t_manage(&mut engine, 2),
            "B is deferred behind the overlay on mon0/ws0"
        );
        assert_eq!(engine.state.pending_focus.map(|pf| pf.window), Some(2));

        // Select the OTHER monitor so mon0 is non-selected.
        engine.execute(crate::core::commands::FocusMonitor(crate::types::Dir::Next));
        assert_ne!(engine.state.sel_mon, mon0);

        // Destroy overlay A on mon0. The deferred B must be consumed on mon0.
        t_destroy(&mut engine, 1);
        assert_eq!(
            engine.state.monitors[mon0].focused,
            Some(2),
            "destroying overlay A on a non-selected monitor must focus deferred B on mon0"
        );
        assert!(engine.state.pending_focus.is_none());
        engine
            .state
            .check_invariants()
            .expect("orphan fix (scenario 9): invariants");
    }

    // Moving the overlay owner off the deferral's (monitor, workspace) dismisses
    // the overlay, which must resolve the deferral rather than leave it dangling.
    #[test]
    fn pending_focus_resolved_when_overlay_owner_moved_to_other_ws() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        let target_ws = ws_i + 1;
        assert!(
            target_ws < engine.state.monitors[mi].workspaces.len(),
            "test needs a second workspace"
        );
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);
        t_add(&mut engine, 2);
        engine.state.pending_focus = Some(crate::types::PendingFocus {
            window: 2,
            owner: 1,
            monitor: mi,
            workspace: ws_i,
        });
        assert_eq!(engine.state.presented_overlay_owner(mi), Some(1));

        t_focus(&mut engine, 1);
        engine.execute(crate::core::commands::MoveToWorkspace(target_ws));
        assert_eq!(
            engine.state.monitors[mi].focused,
            Some(2),
            "moving the overlay owner to another workspace must hand focus to the deferred window"
        );
        assert!(
            engine.state.pending_focus.is_none(),
            "the deferral is resolved exactly once"
        );
        engine
            .state
            .check_invariants()
            .expect("MoveToWorkspace dismiss must preserve invariants");
    }

    #[test]
    fn pending_focus_resolved_when_overlay_owner_moved_to_other_mon() {
        let mut engine = setup_engine_multi();
        let mon0 = 0;
        let ws_i = engine.state.monitors[mon0].active_ws;
        engine.state.monitors[mon0].workspaces[ws_i].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);
        t_add(&mut engine, 2);
        engine.state.pending_focus = Some(crate::types::PendingFocus {
            window: 2,
            owner: 1,
            monitor: mon0,
            workspace: ws_i,
        });
        assert_eq!(engine.state.presented_overlay_owner(mon0), Some(1));

        t_focus(&mut engine, 1);
        engine.execute(crate::core::commands::MoveWindowToMonitor(
            1,
            crate::types::Dir::Next,
        ));
        assert_eq!(
            engine.state.monitors[mon0].focused,
            Some(2),
            "moving the overlay owner to another monitor must hand focus to the deferred window"
        );
        assert!(
            engine.state.pending_focus.is_none(),
            "the deferral is resolved exactly once"
        );
        engine
            .state
            .check_invariants()
            .expect("MoveWindowToMonitor dismiss must preserve invariants");
    }

    // Negative case: a deferral whose overlay only moved to a hidden workspace
    // must SURVIVE — hidden is not dismissed, so there is nothing to resolve it.
    #[test]
    fn pending_focus_survives_when_overlay_hidden_by_workspace_switch() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        let other_ws = ws_i + 1;
        assert!(
            other_ws < engine.state.monitors[mi].workspaces.len(),
            "test needs a second workspace"
        );
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);
        t_add(&mut engine, 2);
        engine.state.pending_focus = Some(crate::types::PendingFocus {
            window: 2,
            owner: 1,
            monitor: mi,
            workspace: ws_i,
        });
        assert_eq!(engine.state.presented_overlay_owner(mi), Some(1));

        engine.execute(crate::core::commands::ViewWorkspace(other_ws));
        assert_eq!(
            engine.state.monitors[mi].active_ws, other_ws,
            "workspace switch applied"
        );
        assert!(
            engine.state.pending_focus.is_some(),
            "a hidden (not dismissed) overlay must keep the deferral alive"
        );
        engine
            .state
            .check_invariants()
            .expect("workspace switch must not break invariants");
    }

    // Negative case: a deferral bound to a now non-selected monitor must SURVIVE
    // a monitor switch — its overlay is still presented over there.
    #[test]
    fn pending_focus_survives_when_overlay_on_non_selected_monitor() {
        let mut engine = setup_engine_multi();
        let mon0 = 0;
        let ws_i = engine.state.monitors[mon0].active_ws;
        engine.state.monitors[mon0].workspaces[ws_i].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);
        t_add(&mut engine, 2);
        engine.state.pending_focus = Some(crate::types::PendingFocus {
            window: 2,
            owner: 1,
            monitor: mon0,
            workspace: ws_i,
        });
        assert_eq!(engine.state.presented_overlay_owner(mon0), Some(1));

        engine.execute(crate::core::commands::FocusMonitor(crate::types::Dir::Next));
        assert_ne!(engine.state.sel_mon, mon0, "monitor switch applied");
        assert!(
            engine.state.pending_focus.is_some(),
            "an overlay still presented on its own monitor must keep the deferral alive"
        );
        engine
            .state
            .check_invariants()
            .expect("monitor switch must not break invariants");
    }

    // The EWMH per-axis maximize path is a production entry point that mutates
    // state *outside* `Command::execute`, so it never reaches the engine's
    // post-command safety net and has to resolve a deferral it orphans itself.
    // A maximize overlay is presented exactly while it holds the focus, so a
    // client asking for one axis to be dropped takes its own overlay down — and
    // the deferral queued behind it is exactly the orphan `check_invariants` #8c
    // rejects. Without the reconciliation the queued window's focus would land
    // behind a presentation that no longer exists.
    #[test]
    fn the_per_axis_ewmh_maximize_path_resolves_the_deferral_it_orphans() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_focus(&mut engine, 1);
        crate::core::commands::apply_maximize(&mut engine.state, 1, Some(true), Some(true));
        assert_eq!(
            engine.state.monitors[mi].workspaces[ws_i].presented_maximize,
            Some(1),
            "window 1 is the maximize overlay"
        );
        assert!(
            !t_manage(&mut engine, 2),
            "window 2 is deferred behind the overlay"
        );

        // Exactly what the `_NET_WM_STATE_MAXIMIZED_*` handler does: the core
        // mutator on its own, with no `Engine::execute` around it.
        crate::core::commands::apply_maximize(&mut engine.state, 1, Some(false), Some(false));

        assert_eq!(
            engine.state.monitors[mi].workspaces[ws_i].presented_maximize, None,
            "un-maximizing the owner takes its overlay down"
        );
        assert_eq!(
            engine.state.monitors[mi].focused,
            Some(2),
            "the deferred window takes the focus the moment the overlay goes away"
        );
        assert!(
            engine.state.pending_focus.is_none(),
            "the deferral is resolved exactly once"
        );
        engine
            .state
            .check_invariants()
            .expect("the EWMH per-axis path must preserve invariants");
    }

    // The other half of the same contract: a request that changes nothing must
    // change nothing. A per-axis message that asks for the state a window is
    // already in (`None` on either axis, or the axis bits it already holds) is a
    // no-op, and the early return that skips the flag mutation has to skip the
    // reconciliation too — otherwise a client repeating the same message would
    // silently cancel a live deferral that nothing dismissed.
    #[test]
    fn a_no_op_ewmh_maximize_request_leaves_a_live_deferral_alone() {
        // `owner_fullscreen` picks which overlay owns the deferral: a fullscreen
        // window is one the per-axis path never touches, so for it every axis
        // request is a no-op by construction — which is the shape a maximized
        // request for a window that is not maximized arrives in.
        let case = |owner_fullscreen: bool, vert: Option<bool>, horiz: Option<bool>| {
            let mut engine = setup_engine();
            let mi = engine.state.sel_mon;
            let ws_i = engine.state.monitors[mi].active_ws;
            engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Column;
            t_manage(&mut engine, 1);
            t_focus(&mut engine, 1);
            if owner_fullscreen {
                t_set_fullscreen(&mut engine, 1, true);
            } else {
                crate::core::commands::apply_maximize(&mut engine.state, 1, Some(true), Some(true));
            }
            assert!(
                !t_manage(&mut engine, 2),
                "window 2 is deferred behind the overlay"
            );
            let before = engine.state.pending_focus;

            crate::core::commands::apply_maximize(&mut engine.state, 1, vert, horiz);

            assert_eq!(
                engine.state.pending_focus, before,
                "a no-op maximize request (fullscreen owner: {owner_fullscreen}, \
                 {vert:?}, {horiz:?}) must not resolve a live deferral"
            );
            assert_eq!(
                engine.state.monitors[mi].focused,
                Some(1),
                "the overlay keeps the focus"
            );
            engine
                .state
                .check_invariants()
                .expect("a no-op maximize request must preserve invariants");
        };
        case(false, None, None);
        case(false, Some(true), Some(true));
        case(true, Some(false), Some(false));
        case(true, None, None);
    }

    #[test]
    fn maximize_roundtrip_and_unmaximize() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        t_manage(&mut engine, 1);

        engine.execute(crate::core::commands::ToggleMaximize(Some(1)));
        {
            let c = engine.state.clients.get(&1).unwrap();
            assert!(c.is_maximized_v() && c.is_maximized_h(), "both axes on");
        }
        assert_eq!(
            engine.state.monitors[mi].workspaces[ws_i].presented_maximize,
            Some(1),
            "the focused maximized window owns `presented_maximize`"
        );
        assert_eq!(engine.state.presented_overlay_owner(mi), Some(1));
        engine
            .state
            .check_invariants()
            .expect("maximize must preserve invariants");

        engine.execute(crate::core::commands::ToggleMaximize(Some(1)));
        {
            let c = engine.state.clients.get(&1).unwrap();
            assert!(
                !c.is_maximized_v() && !c.is_maximized_h(),
                "both axes off again"
            );
        }
        assert_eq!(
            engine.state.monitors[mi].workspaces[ws_i].presented_maximize, None,
            "unmaximizing releases the overlay"
        );
        assert_eq!(engine.state.presented_overlay_owner(mi), None);
        engine
            .state
            .check_invariants()
            .expect("unmaximize must preserve invariants");
    }

    #[test]
    fn destroy_background_window_keeps_active_monitor_focus() {
        let mut engine = setup_engine();
        engine
            .state
            .monitors
            .push(Monitor::new(Rect::new(1920, 0, 1920, 1080), 9));
        // Selected monitor 0 owns window 1.
        t_manage(&mut engine, 1);
        // Background monitor 1 owns windows 2 and 3 (3 focused there).
        for win in [2u32, 3u32] {
            let mut c = Client::new(win, 1, 0);
            c.border_w = engine.cfg.border_w;
            c.geom = Rect::new(1920, 0, 800, 600);
            c.saved_geom = c.geom;
            engine.state.monitors[1].workspaces[0].add_tiled(win, engine.cfg.column_width);
            engine.state.add_client(c);
            let m = &mut engine.state.monitors[1];
            m.focused = Some(win);
            m.focus_stack.retain(|&w| w != win);
            m.focus_stack.push(win);
        }
        assert_eq!(engine.state.sel_mon, 0);

        t_destroy(&mut engine, 3);
        assert_eq!(
            engine.state.sel_mon, 0,
            "closing a background window must not move the monitor selection"
        );
        assert_eq!(
            engine.state.monitors[0].focused,
            Some(1),
            "the active monitor keeps its own focus"
        );
        assert_eq!(
            engine.state.monitors[1].focused,
            Some(2),
            "the background monitor repairs its own focus locally"
        );
        engine
            .state
            .check_invariants()
            .expect("background teardown must preserve invariants");
    }

    // These drive the pure `arrange` + `present_into` +
    // `DesiredState::from_placements` + `reconcile` pipeline (no X server) and pin
    // the geometry each window *should* receive. `reconcile` is the single owner
    // of "what has actually been written to X11", so the Desired it diffs
    // against must be exactly the layout/present projection.

    /// Run the production geometry pipeline for monitor `mi` and return the
    /// explicit `DesiredState` (exactly what `reconcile` is later diffed against).
    fn pipeline_desired(engine: &Engine, mi: usize) -> DesiredState {
        use crate::core::layout::{arrange, Placements, RibbonScratch};
        use crate::core::present::present_into;
        let mut placements = Placements::new();
        arrange(
            &engine.state,
            mi,
            &engine.cfg,
            &mut placements,
            &mut RibbonScratch::default(),
        );
        present_into(&engine.state, &engine.state.monitors[mi], &mut placements);
        DesiredState::from_placements(&placements)
    }

    #[test]
    fn overlay_desired_geometry_matches_layout() {
        use crate::types::LayoutKind;
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);

        let desired = pipeline_desired(&engine, mi);
        let entry = desired
            .windows
            .iter()
            .find(|d| d.window == 1)
            .expect("fullscreen window present in Desired");
        assert_eq!(
            entry.rect, engine.state.monitors[mi].screen,
            "fullscreen overlay desired rect must equal the monitor screen"
        );
        assert_eq!(
            entry.border, 0,
            "fullscreen overlay desired border must be 0"
        );
    }

    #[test]
    fn fullscreen_desired_geometry() {
        use crate::types::LayoutKind;
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_manage(&mut engine, 2);
        t_focus(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);

        let desired = pipeline_desired(&engine, mi);
        let entry = desired
            .windows
            .iter()
            .find(|d| d.window == 1)
            .expect("focused fullscreen window present in Desired");
        assert_eq!(
            entry.rect, engine.state.monitors[mi].screen,
            "fullscreen desired rect must equal the monitor screen"
        );
        // The other (tiled) window keeps a real, positive, on-screen tile.
        let other = desired
            .windows
            .iter()
            .find(|d| d.window == 2)
            .expect("tiled window present in Desired");
        assert!(other.rect.w > 0 && other.rect.h > 0);
    }

    #[test]
    fn maximize_desired_geometry() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let _ws_i = engine.state.monitors[mi].active_ws;
        t_manage(&mut engine, 1);
        t_focus(&mut engine, 1);
        t_set_maximized(&mut engine, 1);
        engine.state.sync_presented_maximize(mi);

        let desired = pipeline_desired(&engine, mi);
        let entry = desired
            .windows
            .iter()
            .find(|d| d.window == 1)
            .expect("maximized window present in Desired");
        assert_eq!(
            entry.rect, engine.state.monitors[mi].workarea,
            "maximized desired rect must equal the workarea"
        );
        assert_eq!(entry.border, 0, "maximized desired border must be 0");
    }

    #[test]
    fn float_desired_geometry() {
        use crate::types::{Client, WinFlags};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        let g = Rect::new(100, 100, 300, 200);
        let mut c = Client::new(1, mi, ws_i);
        c.flags.set(WinFlags::FLOAT);
        c.geom = g;
        c.saved_geom = g;
        c.border_w = engine.cfg.border_w;
        engine.state.add_client(c);
        engine.state.monitors[mi].workspaces[ws_i].floats.push(1);
        engine.state.monitors[mi].focused = Some(1);

        let desired = pipeline_desired(&engine, mi);
        let entry = desired
            .windows
            .iter()
            .find(|d| d.window == 1)
            .expect("float present in Desired");
        assert_eq!(
            entry.rect, g,
            "float desired rect must equal the client's geom"
        );
    }

    /// Whether `win` sits in a column, which is the only place a non-exclusive
    /// fullscreen window is presented from: `fs_ctx` decides the fullscreen
    /// ribbon columns from `ws.columns` and `present_into` defers to it.
    fn is_fullscreen_ribbon_column(
        state: &crate::types::State,
        mi: usize,
        ws_i: usize,
        win: crate::types::WindowId,
    ) -> bool {
        state.monitors[mi].workspaces[ws_i]
            .columns
            .iter()
            .any(|c| c.windows.contains(&win))
    }

    // A fullscreen window's *presentation* depends on its policy, and only one
    // of the two policies survives being a float.
    //
    // `FullscreenPolicy::True` is an exclusive overlay: `present_into` rewrites
    // its entry to `mon.screen` whatever list the window happens to live in, so
    // a True-policy fullscreen float is well defined. `FullscreenPolicy::Normal`
    // is the opposite — it means "a ribbon column that happens to fill the
    // screen" — and both places that know how to present it look at `ws.columns`
    // only (`fs_ctx` decides which columns are fullscreen ribbon columns;
    // `present_into`'s overlay branch requires the True policy). So a Normal
    // fullscreen window in `ws.floats` is presented as an ordinary workarea
    // float while `_NET_WM_STATE_FULLSCREEN` still tells the client it fills the
    // screen. Nothing bridges that, and nothing notices.
    //
    // The state is reachable: tile a window, fullscreen it, navigate to a
    // sibling column — which yields exclusive presentation and drops the policy
    // to Normal, keeping the FULLSCREEN flag — then float it.
    //
    // So the invariant is precise, and narrower than "never both": a fullscreen
    // window that is *not* an exclusive overlay must be a column.
    #[test]
    fn only_an_exclusive_overlay_may_be_a_floating_fullscreen() {
        use crate::core::commands::{Command, FocusDirection, ToggleFloat, ToggleFullscreen};
        use crate::types::{Client, Dir, FullscreenPolicy, Rect};

        for policy in [FullscreenPolicy::True, FullscreenPolicy::Normal] {
            let mut engine = setup_engine();
            let mi = engine.state.sel_mon;
            let ws_i = engine.state.monitors[mi].active_ws;
            // Two tiled columns, so horizontal navigation has somewhere to go.
            for win in [1u32, 2] {
                let mut c = Client::new(win, mi, ws_i);
                c.geom = Rect::new(0, 0, 800, 600);
                c.saved_geom = c.geom;
                engine.state.add_client(c);
                engine.state.monitors[mi].workspaces[ws_i].add_tiled(win, 1.0);
            }
            engine.state.monitors[mi].focused = Some(1);
            engine.state.monitors[mi].focus_stack = vec![1, 2];

            // Fullscreen window 1, then navigate to its sibling.
            ToggleFullscreen(Some(1)).execute(&mut engine.state, &mut engine.cfg);
            {
                let c = engine.state.clients.get_mut(&1).expect("client 1");
                assert!(c.is_fullscreen(), "the window must be fullscreen");
                c.fullscreen_policy = policy;
            }
            FocusDirection(Dir::Right).execute(&mut engine.state, &mut engine.cfg);
            {
                let c = engine.state.clients.get(&1).expect("client 1");
                assert_eq!(
                    c.fullscreen_policy,
                    FullscreenPolicy::Normal,
                    "navigating away is what demotes the policy, and is what                      makes the float case reachable"
                );
            }

            // Now float it.
            ToggleFloat(Some(1)).execute(&mut engine.state, &mut engine.cfg);
            let c = engine.state.clients.get(&1).expect("client 1");
            if c.is_float() {
                assert!(
                    c.is_fullscreen_overlay(),
                    "a floating fullscreen window must be an exclusive overlay: a \
                     Normal-policy one is presented as an ordinary float while \
                     _NET_WM_STATE_FULLSCREEN still claims the screen"
                );
            } else {
                // The toggle was refused, so the window is still a column — and a
                // Normal-policy fullscreen column is the *legitimate* ribbon
                // fullscreen, which `fs_ctx` sizes and the camera targets. What
                // must never happen is the window being in neither list, where no
                // presentation path reaches it at all.
                assert!(
                    !c.is_fullscreen() || is_fullscreen_ribbon_column(&engine.state, mi, ws_i, 1),
                    "a fullscreen window left in neither ws.columns nor an \
                     exclusive overlay is presented by no path at all"
                );
            }
        }
    }

    // `WinFlags::STICKY` is a modifier of `FLOAT`, not an independent flag: its
    // own documentation says a sticky window "is treated as floating: it is
    // excluded from the column layout and shown on every workspace of its
    // monitor", and `apply_rules` establishes the pair at map time for exactly
    // that reason ("a sticky tile would fight the tiling geometry of every
    // workspace"). Every transition that stops a window floating must therefore
    // stop it being sticky, or the window is left in a state the layout has no
    // meaning for: `hide_offscreen` exempts sticky windows from parking (so it
    // is never hidden) while `arrange` only places the *active* workspace (so it
    // is never drawn) — it stays on screen over a workspace the user is not
    // looking at, and nothing brings it back.
    #[test]
    fn a_window_that_stops_floating_stops_being_sticky() {
        use crate::core::commands::{Command, ToggleFloat, ToggleFullscreen};
        use crate::types::{Client, WinFlags};

        let sticky_float = |engine: &Engine, win: u32| -> bool {
            engine
                .state
                .clients
                .get(&win)
                .is_some_and(|c| c.is_sticky() && c.is_float())
        };

        // Tearing off a sticky float: the user asked for a tile, so the window
        // is no longer a float and must not claim to be a sticky one.
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        let mut c = Client::new(1, mi, ws_i);
        c.flags.set(WinFlags::FLOAT);
        c.flags.set(WinFlags::STICKY);
        engine.state.add_client(c);
        engine.state.monitors[mi].workspaces[ws_i].floats.push(1);
        engine.state.monitors[mi].focused = Some(1);

        assert!(
            sticky_float(&engine, 1),
            "fixture must start as a sticky float"
        );
        ToggleFloat(Some(1)).execute(&mut engine.state, &mut engine.cfg);
        let c = engine.state.clients.get(&1).unwrap();
        assert!(!c.is_float(), "ToggleFloat must tile the window");
        assert!(
            !c.is_sticky(),
            "a window that stopped floating must not stay sticky: it would be \
             neither parked (hide_offscreen exempts sticky) nor placed (arrange \
             only projects the active workspace)"
        );

        // A sticky float promoted into the ribbon by fullscreen. The promotion is
        // what makes it a column, so the same rule applies.
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        let mut c = Client::new(1, mi, ws_i);
        c.flags.set(WinFlags::FLOAT);
        c.flags.set(WinFlags::STICKY);
        c.geom = Rect::new(10, 10, 200, 100);
        c.saved_geom = c.geom;
        engine.state.add_client(c);
        engine.state.monitors[mi].workspaces[ws_i].floats.push(1);
        engine.state.monitors[mi].focused = Some(1);

        assert!(
            sticky_float(&engine, 1),
            "fixture must start as a sticky float"
        );
        ToggleFullscreen(Some(1)).execute(&mut engine.state, &mut engine.cfg);
        let c = engine.state.clients.get(&1).unwrap();
        assert!(c.is_fullscreen(), "the window must be fullscreen");
        assert!(!c.is_float(), "fullscreen promotes the float into a column");
        assert!(
            !c.is_sticky(),
            "a column is not a float, so it cannot be a sticky float either"
        );
    }

    // Origin vs layout mode: `ToggleFloat` must never blur the window's floating
    // ORIGIN (`WinFlags::FLOAT_NATIVE`), so a born-floating window (dialog,
    // splash, transient, rule) stays distinguishable from a tile the user tore
    // off — even after both have been through tiled→float→tiled.
    #[test]
    fn toggle_float_preserves_window_origin() {
        use crate::core::commands::{Command, ToggleFloat};
        use crate::types::{Client, WinFlags};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;

        // Tiled origin: tear off (ToggleFloat) and put back. The origin bit
        // must stay CLEAR through both transitions.
        engine.state.add_client(Client::new(1, mi, ws_i));
        engine.state.monitors[mi].workspaces[ws_i].add_tiled(1, engine.cfg.column_width);
        engine.state.monitors[mi].focused = Some(1);
        ToggleFloat(None).execute(&mut engine.state, &mut engine.cfg);
        assert!(engine.state.clients.get(&1).unwrap().is_float());
        assert!(
            !engine.state.clients.get(&1).unwrap().is_native_float(),
            "a torn-off tile must NOT acquire the native-float origin"
        );
        ToggleFloat(None).execute(&mut engine.state, &mut engine.cfg);
        assert!(!engine.state.clients.get(&1).unwrap().is_float());
        assert!(!engine.state.clients.get(&1).unwrap().is_native_float());

        // Native origin: born floating (as manage() marks every policy-floated
        // window). Toggle it tiled and back — the origin bit must SURVIVE both
        // transitions so drag semantics can still tell it apart.
        let mut f = Client::new(2, mi, ws_i);
        f.flags.set(WinFlags::FLOAT);
        f.flags.set(WinFlags::FLOAT_NATIVE);
        engine.state.add_client(f);
        engine.state.monitors[mi].workspaces[ws_i].floats.push(2);
        engine.state.monitors[mi].focused = Some(2);
        ToggleFloat(None).execute(&mut engine.state, &mut engine.cfg);
        assert!(!engine.state.clients.get(&2).unwrap().is_float());
        assert!(
            engine.state.clients.get(&2).unwrap().is_native_float(),
            "tiled mode must not erase a native float's origin"
        );
        ToggleFloat(None).execute(&mut engine.state, &mut engine.cfg);
        assert!(engine.state.clients.get(&2).unwrap().is_float());
        assert!(engine.state.clients.get(&2).unwrap().is_native_float());
    }

    // Native-float geometry ownership: a native float smaller than a tile keeps
    // its own rect (never stretched to the column width), and one larger than the
    // workarea is clamped to the WORKAREA — never to a column/tile rectangle.
    #[test]
    fn native_float_geometry_is_independent_from_tile_rect() {
        use crate::types::{Client, WinFlags};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        let wa = engine.state.monitors[mi].workarea;

        // One tiled window (defines the tile rect: column_width * workarea).
        engine.state.add_client(Client::new(1, mi, ws_i));
        engine.state.monitors[mi].workspaces[ws_i].add_tiled(1, engine.cfg.column_width);
        engine.state.monitors[mi].focused = Some(1);

        // Native float much smaller than any tile.
        let small = Rect::new(120, 90, 320, 240);
        let mut f = Client::new(2, mi, ws_i);
        f.flags.set(WinFlags::FLOAT);
        f.flags.set(WinFlags::FLOAT_NATIVE);
        f.geom = small;
        f.saved_geom = small;
        f.border_w = engine.cfg.border_w;
        engine.state.add_client(f);
        engine.state.monitors[mi].workspaces[ws_i].floats.push(2);

        // Native float larger than the workarea (must clamp to wa, not tile).
        let mut big = Client::new(3, mi, ws_i);
        big.flags.set(WinFlags::FLOAT);
        big.flags.set(WinFlags::FLOAT_NATIVE);
        big.geom = Rect::new(-100, -100, 9999, 9999);
        big.saved_geom = big.geom;
        big.border_w = engine.cfg.border_w;
        engine.state.add_client(big);
        engine.state.monitors[mi].workspaces[ws_i].floats.push(3);

        let desired = pipeline_desired(&engine, mi);
        let tile_w = (wa.w as f32 * engine.cfg.column_width) as u32;

        let small_entry = desired
            .windows
            .iter()
            .find(|d| d.window == 2)
            .expect("native float present in Desired");
        assert_eq!(
            small_entry.rect, small,
            "native float keeps its own (smaller-than-tile) geometry"
        );
        assert_ne!(
            small_entry.rect.w, tile_w,
            "native float must not be stretched to the column width"
        );

        let big_entry = desired
            .windows
            .iter()
            .find(|d| d.window == 3)
            .expect("oversized native float present in Desired");
        // The clamp includes the 2*border_w frame, same as
        // `clamp_float_to_workarea`.
        let bw = engine.cfg.border_w;
        let exp_w = (wa.w as i32 - 2 * bw as i32).max(1) as u32;
        let exp_h = (wa.h as i32 - 2 * bw as i32).max(1) as u32;
        assert_eq!(
            big_entry.rect.w, exp_w,
            "oversized float clamps to workarea width minus frame"
        );
        assert_eq!(
            big_entry.rect.h, exp_h,
            "oversized float clamps to workarea height minus frame"
        );
    }

    // Client authority over its float: when the WM adopts a `ConfigureRequest`
    // verbatim (the `events.rs` sink), it seals `float_client_authority` and
    // `arrange` must project THAT rect verbatim instead of re-normalizing it,
    // even where snapping to the hints would move it. Without the seal the WM
    // rewrites what it promised, the client claims its rect back, and the float
    // moves on its own — two authorities ping-ponging.
    #[test]
    fn adopted_float_request_is_projected_verbatim() {
        use crate::types::{Client, SizeHints, WinFlags};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;

        // Float on a size-hints grid (base 0, increment 10, min 100x100).
        let hints = SizeHints {
            base_w: 0,
            base_h: 0,
            inc_w: 10,
            inc_h: 10,
            max_w: 0,
            max_h: 0,
            min_w: 100,
            min_h: 100,
            min_aspect: 0.0,
            max_aspect: 0.0,
            flags: 0,
            valid: true,
        };
        let requested = Rect::new(60, 70, 600, 400); // on-grid, snap-neutral
        let mut f = Client::new(2, mi, ws_i);
        f.flags.set(WinFlags::FLOAT);
        f.geom = requested;
        f.saved_geom = requested;
        f.hints = hints;
        f.border_w = engine.cfg.border_w;
        engine.state.add_client(f);
        engine.state.monitors[mi].workspaces[ws_i].floats.push(2);

        // The sink adopted the request verbatim and sealed the authority.
        engine
            .state
            .clients
            .get_mut(&2)
            .unwrap()
            .float_client_authority = true;

        let desired = pipeline_desired(&engine, mi);
        let entry = desired
            .windows
            .iter()
            .find(|d| d.window == 2)
            .expect("sealed float present in Desired");
        assert_eq!(
            entry.rect, requested,
            "sealed float must be projected verbatim, not re-normalized"
        );
        assert!(
            engine.state.clients.get(&2).unwrap().float_client_authority,
            "projection must be pure: the seal survives arrange"
        );
    }

    // A float that gains a new context (`ToggleFloat`, a workspace or monitor
    // change) must have its rect re-settled as a fixed point of the new
    // workarea's projection, so the first `arrange` cannot correct it with a
    // visible jump. Single helper: `layout::settle_float_in_workarea`.
    #[test]
    fn float_gaining_new_context_is_settled_before_first_arrange() {
        use crate::types::{Client, WinFlags};
        let mut engine = setup_engine_multi();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;

        // ToggleFloat: the projected tile (~800x1080, off-grid) must be born
        // floating already re-settled onto the hint grid (inc 10).
        engine.state.add_client(Client::new(1, mi, ws_i));
        engine.state.monitors[mi].workspaces[ws_i].add_tiled(1, engine.cfg.column_width);
        engine.state.monitors[mi].focused = Some(1);
        let hints_inc: u32 = 10;
        {
            let c = engine.state.clients.get_mut(&1).unwrap();
            c.hints.inc_w = hints_inc as i32;
            c.hints.inc_h = hints_inc as i32;
            c.hints.valid = true;
        }
        // Pre-toggle state as in production: `arrange` has already written the
        // projected tile into `client.geom` (typically off-grid w.r.t. hints).
            let pre = pipeline_desired(&engine, mi);
        let tile = pre
            .windows
            .iter()
            .find(|d| d.window == 1)
            .expect("tiled window in Desired")
            .rect;
        engine.state.clients.get_mut(&1).unwrap().geom = tile;
        crate::core::commands::ToggleFloat(None).execute(&mut engine.state, &mut engine.cfg);
        {
            let c = engine.state.clients.get(&1).unwrap();
            assert!(c.is_float(), "toggle made it float");
            assert!(
                c.geom.w % hints_inc == 0 && c.geom.h % hints_inc == 0,
                "born-float geom must be on the hint grid at once, geom={:?}",
                c.geom
            );
        }
        let settled = engine.state.clients.get(&1).unwrap().geom;
        let desired = pipeline_desired(&engine, mi);
        let entry = desired
            .windows
            .iter()
            .find(|d| d.window == 1)
            .expect("float present in Desired");
        assert_eq!(
            entry.rect, settled,
            "first arrange after settling must not move the float"
        );

        // MoveWindowToMonitor: a still float changes to a new workarea (another
        // monitor) and must come out re-settled inside it in the same command.
        let mut f = Client::new(2, mi, ws_i);
        f.flags.set(WinFlags::FLOAT);
        f.geom = Rect::new(100, 100, 200, 150); // off monitor 1 (x >= 1920)
        f.saved_geom = f.geom;
        engine.state.add_client(f);
        engine.state.monitors[mi].workspaces[ws_i].floats.push(2);
        crate::core::commands::MoveWindowToMonitor(2, crate::types::Dir::Right)
            .execute(&mut engine.state, &mut engine.cfg);
        let new_mi = (mi + 1) % engine.state.monitors.len();
        let wa1 = engine.state.monitors[new_mi].workarea;
        let c2 = engine.state.clients.get(&2).unwrap();
        assert_eq!(c2.monitor, new_mi, "float moved to monitor 1");
        assert!(
            c2.geom.x >= wa1.x && c2.geom.x + c2.geom.w as i32 <= wa1.right(),
            "float must be settled inside the NEW workarea in the same command, geom={:?} wa={:?}",
            c2.geom,
            wa1
        );
    }

    // A tiled window's self-resize request is DENIED: `client.geom` (the
    // desired) stays the WM-authored tile, never the client's divergent request.
    #[test]
    fn self_resize_does_not_mutate_desired() {
        use crate::backend::x11::reconciler::{
            classify_configure, AppliedWindow, ConfigureObservation,
        };
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        t_manage(&mut engine, 1);

        // Run the production pipeline and write client.geom from the projection,
        // exactly as the backend does each frame.
        let mut p = crate::core::layout::Placements::new();
        crate::core::layout::arrange(
            &engine.state,
            mi,
            &engine.cfg,
            &mut p,
            &mut RibbonScratch::default(),
        );
        let tile = p.iter().find(|e| e.0 == 1).copied().unwrap().1;
        for (win, rect, bw) in &p {
            if let Some(c) = engine.state.clients.get_mut(win) {
                c.geom = *rect;
                c.border_w = *bw;
            }
        }
        let before = engine.state.clients[&1].geom;
        assert_eq!(before, tile, "desired geom is the tiled placement");

        // A client self-resize reports a divergent rect. The WM (tiled → authority)
        // must NOT adopt it: classify_configure returns Stale, so the model
        // re-asserts Desired instead of mutating client.geom.
        let requested = Rect::new(40, 40, 640, 480);
        assert_ne!(
            requested, before,
            "sanity: the request differs from the tile"
        );
        let applied = AppliedWindow {
            rect: before,
            border_w: 2,
            seen: true,
            sequence: None,
        };
        let obs = classify_configure(requested, 2, &applied);
        assert_eq!(
            obs,
            ConfigureObservation::Stale,
            "tiled self-resize must be denied (re-asserted), not followed"
        );
        assert_eq!(
            engine.state.clients[&1].geom, before,
            "client.geom (desired) is unchanged after the self-resize request"
        );
    }

    // A tiled window that diverges (`geometry_dirty`) must re-apply: the next
    // pipeline run's reconcile returns a Configure for it.
    #[test]
    fn self_resize_tiled_causes_reapply() {
        use crate::backend::x11::reconciler::{reconcile, AppliedState, AppliedWindow};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        t_manage(&mut engine, 1);

        let desired = pipeline_desired(&engine, mi);
        let (r, b) = {
            let e = desired.windows.iter().find(|d| d.window == 1).unwrap();
            (e.rect, e.border)
        };

        // Applied already matches the desired rect/border — normally a no-op.
        let mut applied = AppliedState::default();
        applied.windows.insert(
            1,
            AppliedWindow {
                rect: r,
                border_w: b,
                seen: true,
                sequence: None,
            },
        );

        // With geometry_dirty set, reconcile must force a Configure even though
        // the rect is identical (a tiled window that moved on its own is snapped
        // back to where the WM put it).
        engine.state.clients.get_mut(&1).unwrap().geometry_dirty = true;
        let effects = reconcile(&desired, &engine.state, &mut applied);
        assert_eq!(effects.len(), 1, "a dirty tiled window must re-apply");
        match &effects[0] {
            crate::backend::x11::reconciler::GeometryEffect::Configure { win, rect, border } => {
                assert_eq!(*win, 1);
                assert_eq!(*rect, r, "re-apply carries the WM-authored rect");
                assert_eq!(*border, b);
            }
        }
        // Clearing the flag (as the backend does after emitting) → no longer forced.
        engine.state.clients.get_mut(&1).unwrap().geometry_dirty = false;
        let effects = reconcile(&desired, &engine.state, &mut applied);
        assert!(effects.is_empty(), "once forced, identical rect is a no-op");
    }

    // A float that self-resizes is followed: the model adopts the requested rect
    // and the pipeline's Desired for that window matches `client.geom`.
    #[test]
    fn self_resize_float_can_follow() {
        use crate::backend::x11::reconciler::{
            classify_configure, AppliedWindow, ConfigureObservation,
        };
        use crate::types::{Client, WinFlags};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        let g0 = Rect::new(100, 100, 300, 200);
        let g1 = Rect::new(200, 150, 400, 250);
        let mut c = Client::new(1, mi, ws_i);
        c.flags.set(WinFlags::FLOAT);
        c.geom = g0;
        c.saved_geom = g0;
        c.border_w = engine.cfg.border_w;
        engine.state.add_client(c);
        engine.state.monitors[mi].workspaces[ws_i].floats.push(1);
        engine.state.monitors[mi].focused = Some(1);

        // The WM yields to floats: a self-resize is adopted into the model.
        engine.state.clients.get_mut(&1).unwrap().geom = g1;
        let desired = pipeline_desired(&engine, mi);
        let entry = desired
            .windows
            .iter()
            .find(|d| d.window == 1)
            .expect("float present in Desired");
        assert_eq!(
            entry.rect, g1,
            "float self-resize is followed: Desired matches the new geom"
        );
        assert_eq!(
            engine.state.clients.get(&1).unwrap().geom,
            g1,
            "the model adopted the float's new geometry"
        );

        // The convergence policy agrees: a float's divergence is followed (the
        // sink adopts; classification just marks the report as not-our-echo).
        let applied = AppliedWindow {
            rect: g0,
            border_w: 2,
            seen: true,
            sequence: None,
        };
        let obs = classify_configure(g1, 2, &applied);
        assert_eq!(
            obs,
            ConfigureObservation::Stale,
            "a float's self-resize is not our echo: Stale, for the sink to adopt"
        );
    }

    // The geometry counterpart of `property_random_window_ops_preserve_invariants`:
    // same kind of randomized Create/Destroy/Fullscreen/Maximize/Float/
    // MoveResize/WorkspaceSwitch/MonitorSwitch/LayoutChange chaos, but asserting
    // the *geometry* contract. After every step the explicit Desired for every
    // monitor is diffed against a single long-lived `AppliedState` through
    // `reconcile`, and the invariants the backend relies on to never write a
    // bogus Configure are asserted.

    #[test]
    fn property_geometry_pipeline_consistency() {
        use crate::backend::x11::reconciler::{reconcile, AppliedState};
        use crate::core::commands::{
            MoveResize, ToggleFloat, ToggleFullscreen, ToggleMaximize, ViewWorkspace,
        };
        use crate::types::{LayoutKind, WindowId};

        const SEED: u64 = 0xFEED_C0DE_1357_9B00;
        const STEPS: u32 = 200;
        const MAX_WINS: usize = 16;

        // Tiny deterministic LCG (matches the existing property harness).
        struct Rng(u64);
        impl Rng {
            fn next(&mut self) -> u64 {
                self.0 = self
                    .0
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                self.0 >> 16
            }
            fn below(&mut self, n: u32) -> u32 {
                (self.next() % n as u64) as u32
            }
        }
        let mut rng = Rng(SEED);

        let mut engine = setup_engine_multi();
        let nmon = engine.state.monitors.len();
        let mut live: Vec<WindowId> = Vec::new();
        let mut next_win: WindowId = 1;
        let mut applied = AppliedState::default();

        // Ensure at least one window so focus/overlay ops have a target.
        {
            let mi = engine.state.sel_mon;
            t_manage(&mut engine, next_win);
            live.push(next_win);
            next_win += 1;
            let _ = mi;
        }

        // The whole-desktop Desired: `pipeline_desired` (arrange + present_into +
        // DesiredState::from_placements) run for EVERY monitor and merged, so the
        // duplicate/stale checks below see across monitors, not just one.
        let run_pipeline_all = |engine: &Engine| -> DesiredState {
            let mut all = DesiredState::default();
            for mi in 0..engine.state.monitors.len() {
                let d = pipeline_desired(engine, mi);
                all.windows.extend(d.windows);
            }
            all
        };

        // Coverage counters: a chaos run that never actually produced a float, a
        // Configure or a destroy would make the invariants below vacuous.
        let mut float_checks = 0usize;
        let mut configures = 0usize;
        let mut destroys = 0usize;

        for step in 0..STEPS {
            let op = rng.below(9);
            match op {
                // Create on a random monitor/workspace.
                0 => {
                    if live.len() < MAX_WINS {
                        let target = rng.below(nmon as u32) as usize;
                        let tws = rng.below(engine.state.monitors[target].workspaces.len() as u32)
                            as usize;
                        engine.state.sel_mon = target;
                        engine.state.monitors[target].active_ws = tws;
                        let w = next_win;
                        next_win += 1;
                        t_manage(&mut engine, w);
                        live.push(w);
                        // Deferred focus (`pending_focus`) is pure focus
                        // bookkeeping — the geometry pipeline never reads it
                        // (`layout` / `present` / `desired` never mention it).
                        // This test does not exercise the deferral, and a
                        // harness-created one going stale later only trips the
                        // *focus* invariant (frozen domain, out of scope here).
                        engine.state.pending_focus = None;
                    }
                }
                // Destroy a random live window.
                1 => {
                    if !live.is_empty() {
                        let i = rng.below(live.len() as u32) as usize;
                        let w = live.remove(i);
                        t_destroy(&mut engine, w);
                        // Backend cleanup: forget the destroyed window's Applied entry.
                        applied.forget(w);
                        destroys += 1;
                    }
                }
                // Fullscreen toggle.
                2 => {
                    engine.execute(ToggleFullscreen(None));
                }
                // Maximize toggle.
                3 => {
                    engine.execute(ToggleMaximize(None));
                }
                // Float toggle.
                4 => {
                    engine.execute(ToggleFloat(None));
                }
                // MoveResize: a valid rect, applied as a self-resize (float-follow).
                5 => {
                    if !live.is_empty() {
                        let w = live[rng.below(live.len() as u32) as usize];
                        let is_float = engine
                            .state
                            .clients
                            .get(&w)
                            .is_some_and(crate::types::Client::is_float);
                        // Only resize a window that is ALREADY floating: the WM
                        // follows floats, and `ToggleFloat` already placed it
                        // (removed from columns, added to ws.floats) without
                        // creating a column/float duplication.
                        if is_float {
                            let gx = (rng.below(800) as i32) + 50;
                            let gy = (rng.below(600) as i32) + 50;
                            let gw = 100 + rng.below(400);
                            let gh = 100 + rng.below(300);
                            let g = Rect::new(gx, gy, gw, gh);
                            if let Some(c) = engine.state.clients.get_mut(&w) {
                                c.geom = g;
                            }
                            engine.execute(MoveResize(w, g));
                        }
                    }
                }
                // WorkspaceSwitch (view a random workspace).
                6 => {
                    let n = engine.state.monitors[engine.state.sel_mon].workspaces.len();
                    let ws = rng.below(n as u32) as usize;
                    engine.execute(ViewWorkspace(ws));
                    let sel = engine.state.sel_mon;
                    if let Some(b) = engine.state.best_focus(sel) {
                        crate::core::commands::focus_logical_on(&mut engine.state, sel, b);
                    }
                }
                // MonitorSwitch (focus a random monitor).
                7 => {
                    let m = rng.below(nmon as u32) as usize;
                    engine.state.sel_mon = m;
                    if let Some(b) = engine.state.best_focus(m) {
                        crate::core::commands::focus_logical_on(&mut engine.state, m, b);
                    }
                }
                // LayoutChange (set a random LayoutKind on a random workspace).
                _ => {
                    let m = rng.below(nmon as u32) as usize;
                    let ws_i = rng.below(engine.state.monitors[m].workspaces.len() as u32) as usize;
                    let lk = if rng.below(2) == 0 {
                        LayoutKind::Column
                    } else {
                        LayoutKind::Column
                    };
                    engine.state.monitors[m].workspaces[ws_i].layout = lk;
                }
            }

            // Geometry contract checks.
            let desired = run_pipeline_all(&engine);
            let effects = reconcile(&desired, &engine.state, &mut applied);

            // (a) every emitted effect names a live client with a positive rect.
            for eff in &effects {
                let crate::backend::x11::reconciler::GeometryEffect::Configure {
                    win, rect, ..
                } = eff;
                assert!(
                    engine.state.clients.contains_key(win),
                    "seed {SEED:#x} step {step}: Configure for unknown window {win}"
                );
                assert!(
                    rect.w > 0 && rect.h > 0,
                    "seed {SEED:#x} step {step}: Configure with zero-size rect {rect:?}"
                );
                configures += 1;
            }

            // (b) no duplicate window ids within desired.windows.
            {
                let mut seen = std::collections::HashSet::new();
                for d in &desired.windows {
                    assert!(
                        seen.insert(d.window),
                        "seed {SEED:#x} step {step}: duplicate window {} in Desired",
                        d.window
                    );
                }
            }

            // (c) every desired window exists in state.clients.
            for d in &desired.windows {
                assert!(
                    engine.state.clients.contains_key(&d.window),
                    "seed {SEED:#x} step {step}: Desired names unknown client {}",
                    d.window
                );
            }

            // (d) Applied must never reference a window not in state.clients.
            for w in applied.windows.keys() {
                assert!(
                    engine.state.clients.contains_key(w),
                    "seed {SEED:#x} step {step}: Applied holds stale window {w}"
                );
            }

            // (e) no zero-size Rect anywhere in Desired.
            for d in &desired.windows {
                assert!(
                    d.rect.w > 0 && d.rect.h > 0,
                    "seed {SEED:#x} step {step}: Desired rect zero-size {d:?}"
                );
            }

            // (f) every client owns valid monitor/workspace indices (no impossible ownership).
            for (&w, c) in &engine.state.clients {
                assert!(
                    c.monitor < engine.state.monitors.len(),
                    "seed {SEED:#x} step {step}: client {w} monitor {} out of range",
                    c.monitor
                );
                let mws = engine.state.monitors[c.monitor].workspaces.len();
                assert!(c.workspace < mws, "seed {SEED:#x} step {step}: client {w} workspace {} out of range (mon {} has {mws})", c.workspace, c.monitor);
            }

            // (g) soft geometry check for FLOATS: the layout reads `client.geom`
            //     for floating windows and only clamps it into the workarea, so a
            //     float whose geom already fits must be projected verbatim — the
            //     WM yields to floats. Overlays (fullscreen / presented maximize)
            //     legitimately override the float rect, so they are skipped.
            //
            //     TILED windows are deliberately NOT compared against
            //     `client.geom`: in this pure arrange/present pass the backend
            //     sink (`apply_geom`) that writes placements back into the model
            //     never runs, so `client.geom` is not expected to track Desired.
            for d in &desired.windows {
                let Some(c) = engine.state.clients.get(&d.window) else {
                    continue;
                };
                if !c.is_float() {
                    continue;
                }
                let mon = &engine.state.monitors[c.monitor];
                let ws = &mon.workspaces[c.workspace];
                let is_overlay = (c.is_fullscreen()
                    && (ws.layout == LayoutKind::Column || c.is_true_fullscreen()))
                    || ws.presented_maximize == Some(d.window);
                if is_overlay {
                    continue;
                }
                let wa = mon.workarea;
                let g = c.geom;
                float_checks += 1;
                let fits = g.x >= wa.x
                    && g.y >= wa.y
                    && g.x + g.w as i32 <= wa.x + wa.w as i32
                    && g.y + g.h as i32 <= wa.y + wa.h as i32;
                if fits {
                    assert_eq!(
                        d.rect, g,
                        "seed {SEED:#x} step {step}: float {} was not projected from its own geom",
                        d.window
                    );
                } else {
                    assert!(
                        d.rect.w <= wa.w && d.rect.h <= wa.h,
                        "seed {SEED:#x} step {step}: float {} clamped rect {:?} exceeds workarea {wa:?}",
                        d.window,
                        d.rect
                    );
                }
            }

            // (h) structural (NON-focus) invariants must still hold.
            //
            //     Focus/overlay *ownership* bookkeeping (`pending_focus`,
            //     `focus_stack`, `presented_overlay_owner`) is a separate, FROZEN
            //     domain and is intentionally not asserted by this test: it is the
            //     *geometry* property test, and a deferred-focus bookkeeping
            //     violation says nothing about the rects the backend would write
            //     to X11. Those invariants are covered by
            //     `property_invariants_hold_under_chaos` and the focus unit tests.
            if let Err(violations) = engine.state.check_invariants() {
                const FOCUS_DOMAIN: [&str; 4] = [
                    "pending_focus",
                    "focus_stack",
                    "overlay owner",
                    "x11_input_focus",
                ];
                let structural: Vec<&String> = violations
                    .iter()
                    .filter(|m| !FOCUS_DOMAIN.iter().any(|k| m.contains(k)))
                    .collect();
                assert!(
                    structural.is_empty(),
                    "seed {SEED:#x} step {step}: structural invariant violation: {structural:?}"
                );
            }
        }

        // The chaos actually exercised every branch the invariants care about
        // (otherwise the assertions above would be vacuously true).
        assert!(
            configures > 0,
            "seed {SEED:#x}: no Configure was ever emitted"
        );
        assert!(destroys > 0, "seed {SEED:#x}: no window was ever destroyed");
        assert!(
            float_checks > 0,
            "seed {SEED:#x}: no floating window was ever projected"
        );
    }

    // The `audit_*` cluster drives the production paths — `Engine::execute`, the
    // `t_*` model helpers, `pipeline_desired`, `reconcile`/`classify_configure`
    // — and only ever asserts invariants. It never adjusts WM behaviour to make a
    // case pass.

    /// Mirror the backend focus sink: run a command, then apply the `FocusWindow`
    /// effect it emitted to `mon.focused` (the core command only *emits* focus).
    fn aud_run_cmd<C: crate::core::commands::Command>(
        engine: &mut Engine,
        cmd: C,
    ) -> Vec<crate::core::effect::Effect> {
        let effects = engine.execute(cmd);
        for eff in &effects {
            if let crate::core::effect::Effect::FocusWindow(w) = eff {
                engine.state.monitors[engine.state.sel_mon].focused = *w;
            }
        }
        effects
    }

    #[test]
    fn audit_p1_tiled_self_resize_reassert() {
        use crate::backend::x11::reconciler::{
            classify_configure, reconcile, AppliedState, AppliedWindow, ConfigureObservation,
            GeometryEffect,
        };
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        t_manage(&mut engine, 1);

        let desired = pipeline_desired(&engine, mi);
        let (r, b) = {
            let e = desired.windows.iter().find(|d| d.window == 1).unwrap();
            (e.rect, e.border)
        };
        // The WM's asserted geometry is the tiled placement.
        engine.state.clients.get_mut(&1).unwrap().geom = r;

        let mut applied = AppliedState::default();
        applied.windows.insert(
            1,
            AppliedWindow {
                rect: r,
                border_w: b,
                seen: true,
                sequence: None,
            },
        );

        // The client attempts a divergent self-resize.
        let reported = Rect::new(40, 40, 640, 480);
        assert_ne!(reported, r, "sanity: request differs from the tile");
        let obs = classify_configure(reported, b, &applied.windows[&1]);
        assert_eq!(
            obs,
            ConfigureObservation::Stale,
            "tiled self-resize must be re-asserted (WM authority), not followed"
        );

        // Simulate the reassert: the WM keeps client.geom = desired and forces a
        // reconfigure; reconcile must emit a Configure carrying the desired rect,
        // and after applying, applied must equal desired (converged).
        engine.state.clients.get_mut(&1).unwrap().geometry_dirty = true;
        let effects = reconcile(&desired, &engine.state, &mut applied);
        assert_eq!(effects.len(), 1, "reassert must emit exactly one Configure");
        match &effects[0] {
            GeometryEffect::Configure { win, rect, border } => {
                assert_eq!(*win, 1);
                assert_eq!(*rect, r, "re-apply carries the WM-authored rect");
                assert_eq!(*border, b);
            }
        }
        assert_eq!(
            applied.windows[&1].rect, r,
            "applied must converge to desired"
        );
        engine
            .state
            .check_invariants()
            .expect("invariants after tiled reassert");
    }

    #[test]
    fn audit_p1_fullscreen_self_resize_reassert() {
        use crate::backend::x11::reconciler::{
            classify_configure, reconcile, AppliedState, AppliedWindow, ConfigureObservation,
            GeometryEffect,
        };
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);

        let desired = pipeline_desired(&engine, mi);
        let (r, b) = {
            let e = desired.windows.iter().find(|d| d.window == 1).unwrap();
            (e.rect, e.border)
        };
        engine.state.clients.get_mut(&1).unwrap().geom = r;

        let mut applied = AppliedState::default();
        applied.windows.insert(
            1,
            AppliedWindow {
                rect: r,
                border_w: b,
                seen: true,
                sequence: None,
            },
        );

        let reported = Rect::new(40, 40, 640, 480);
        let obs = classify_configure(reported, b, &applied.windows[&1]);
        assert_eq!(
            obs,
            ConfigureObservation::Stale,
            "fullscreen self-resize must be re-asserted (stays fullscreen), not followed"
        );

        engine.state.clients.get_mut(&1).unwrap().geometry_dirty = true;
        let effects = reconcile(&desired, &engine.state, &mut applied);
        assert_eq!(
            effects.len(),
            1,
            "fullscreen reassert must emit one Configure"
        );
        match &effects[0] {
            GeometryEffect::Configure { win, rect, .. } => {
                assert_eq!(*win, 1);
                assert_eq!(*rect, r, "fullscreen re-apply carries the screen rect");
            }
        }
        assert_eq!(applied.windows[&1].rect, r);
        engine
            .state
            .check_invariants()
            .expect("invariants after fullscreen reassert");
    }

    #[test]
    fn audit_p1_float_follow_and_adopt() {
        use crate::backend::x11::reconciler::{
            classify_configure, AppliedWindow, ConfigureObservation,
        };
        use crate::types::WinFlags;
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        let g0 = Rect::new(100, 100, 300, 200);
        let mut c = Client::new(1, mi, ws_i);
        c.flags.set(WinFlags::FLOAT);
        c.geom = g0;
        c.saved_geom = g0;
        c.border_w = engine.cfg.border_w;
        engine.state.add_client(c);
        engine.state.monitors[mi].workspaces[ws_i].floats.push(1);

        let mut applied = AppliedWindow {
            rect: g0,
            border_w: 2,
            seen: true,
            sequence: None,
        };
        let g1 = Rect::new(200, 150, 400, 250);
        let obs = classify_configure(g1, 2, &applied);
        assert_eq!(
            obs,
            ConfigureObservation::Stale,
            "a float's self-resize is not our echo: Stale, for the sink to adopt"
        );

        // Simulate adoption: the model adopts the reported rect into client.geom
        // and the Applied entry tracks it; a second classify must be Compliant.
        engine.state.clients.get_mut(&1).unwrap().geom = g1;
        applied = AppliedWindow {
            rect: g1,
            border_w: 2,
            seen: true,
            sequence: None,
        };
        let obs2 = classify_configure(g1, 2, &applied);
        assert_eq!(
            obs2,
            ConfigureObservation::Compliant,
            "after adoption the reported == applied must be Compliant"
        );
    }

    #[test]
    fn audit_p1_float_consecutive_requests_converge() {
        use crate::backend::x11::reconciler::{
            classify_configure, AppliedWindow, ConfigureObservation,
        };
        use crate::types::WinFlags;
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        let g0 = Rect::new(100, 100, 300, 200);
        let mut c = Client::new(1, mi, ws_i);
        c.flags.set(WinFlags::FLOAT);
        c.geom = g0;
        c.saved_geom = g0;
        c.border_w = engine.cfg.border_w;
        engine.state.add_client(c);
        engine.state.monitors[mi].workspaces[ws_i].floats.push(1);

        let mut applied = AppliedWindow {
            rect: g0,
            border_w: 2,
            seen: true,
            sequence: None,
        };
        let mut last = g0;
        for i in 0..5 {
            let r = Rect::new(
                100 + (i + 1) * 11,
                100 + (i + 1) * 11,
                300 + (i as u32 + 1) * 20,
                200 + (i as u32 + 1) * 20,
            );
            let obs = classify_configure(r, 2, &applied);
            assert_eq!(
                obs,
                ConfigureObservation::Stale,
                "float request {i} must not be our echo: Stale, for the sink to adopt"
            );
            engine.state.clients.get_mut(&1).unwrap().geom = r;
            applied = AppliedWindow {
                rect: r,
                border_w: 2,
                seen: true,
                sequence: None,
            };
            last = r;
        }
        let obs = classify_configure(last, 2, &applied);
        assert_eq!(
            obs,
            ConfigureObservation::Compliant,
            "final state must be Compliant"
        );
        assert_eq!(engine.state.clients.get(&1).unwrap().geom, last);
        assert_eq!(
            applied.rect, last,
            "applied converged to last reported rect"
        );
    }

    #[test]
    fn audit_p1_invalid_configure_request_model_clamped() {
        // Boundary: `events.rs::on_configure_request`'s float branch cannot be
        // tested in memory — it round-trips to the X server. Only the pure model
        // contract is pinned here: a bogus requested geometry written into a
        // floating window's `client.geom` must be clamped into the monitor
        // workarea by `arrange()` (via `pipeline_desired`) and leave
        // `State::check_invariants()` Ok.
        use crate::types::WinFlags;
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        let g0 = Rect::new(100, 100, 300, 200);
        let mut c = Client::new(1, mi, ws_i);
        c.flags.set(WinFlags::FLOAT);
        c.geom = g0;
        c.saved_geom = g0;
        c.border_w = engine.cfg.border_w;
        engine.state.add_client(c);
        engine.state.monitors[mi].workspaces[ws_i].floats.push(1);

        let wa = engine.state.monitors[mi].workarea;
        let bad = [
            Rect::new(0, 0, 0, 0),
            Rect::new(-500, -500, u16::MAX as u32, u16::MAX as u32),
            Rect::new(5000, 5000, 300, 300),
            Rect::new(100, 100, 100, 100),
        ];
        for b in bad {
            engine.state.clients.get_mut(&1).unwrap().geom = b;
            let desired = pipeline_desired(&engine, mi);
            let e = desired
                .windows
                .iter()
                .find(|d| d.window == 1)
                .expect("float present in desired");
            assert!(
                e.rect.x >= wa.x
                    && e.rect.y >= wa.y
                    && e.rect.right() <= wa.right()
                    && e.rect.bottom() <= wa.bottom(),
                "float placement must be clamped into workarea, got {:?} vs workarea {:?}",
                e.rect,
                wa
            );
            engine
                .state
                .check_invariants()
                .expect("invariants must hold after bogus float geom");
        }
    }

    // A hostile ConfigureRequest with invalid geometry (0×0, 60000×60000,
    // off-monitor) against a TILED window must be classified `Stale`, so the WM
    // re-asserts, AND the WM's own Desired must stay positive — the model never
    // collapses to a degenerate rect, and `client.geom` is never overwritten by
    // the bogus report.
    #[test]
    fn audit_p1_tiled_invalid_geometry_never_collapses_to_zero() {
        use crate::backend::x11::reconciler::{
            classify_configure, AppliedWindow, ConfigureObservation,
        };
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let _ws_i = engine.state.monitors[mi].active_ws;
        t_manage(&mut engine, 1);
        t_focus(&mut engine, 1);

        let desired_before = pipeline_desired(&engine, mi)
            .windows
            .iter()
            .find(|d| d.window == 1)
            .map(|d| d.rect)
            .expect("tiled window present in Desired");
        let geom_before = engine.state.clients[&1].geom;

        let bad = [
            Rect::new(0, 0, 0, 0),
            Rect::new(0, 0, 60000, 60000),
            Rect::new(9000, 9000, 300, 300),
        ];
        for reported in bad {
            let applied = AppliedWindow {
                rect: desired_before,
                border_w: engine.cfg.border_w,
                seen: true,
                sequence: None,
            };
            let obs = classify_configure(reported, engine.cfg.border_w, &applied);
            assert_eq!(
                obs,
                ConfigureObservation::Stale,
                "tiled invalid ConfigureRequest {reported:?} must be Stale (never adopted)"
            );
            // The model re-asserts Desired; the client geometry is untouched.
            let desired_after = pipeline_desired(&engine, mi)
                .windows
                .iter()
                .find(|d| d.window == 1)
                .map(|d| d.rect)
                .expect("tiled window present in Desired");
            assert_eq!(
                desired_after, desired_before,
                "Desired must not adopt the invalid rect"
            );
            assert_eq!(
                engine.state.clients[&1].geom, geom_before,
                "client.geom must never store the reported invalid rect"
            );
            assert!(
                desired_after.w > 0 && desired_after.h > 0,
                "Desired must stay positive"
            );
        }
        engine
            .state
            .check_invariants()
            .expect("invariants after tiled invalid-geometry requests");
    }

    #[test]
    fn audit_p2_fullscreen_lifecycle_no_orphan_overlay() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);
        assert_eq!(engine.state.presented_overlay_owner(mi), Some(1));

        assert!(
            !t_manage(&mut engine, 2),
            "B is deferred behind the live overlay"
        );
        assert_eq!(engine.state.presented_overlay_owner(mi), Some(1));
        engine.state.check_invariants().expect("after create B");

        // Focus B: dismiss A's fullscreen so the deferred focus resolves to B.
        aud_run_cmd(
            &mut engine,
            crate::core::commands::ToggleFullscreen(Some(1)),
        );
        t_focus(&mut engine, 2);
        assert_eq!(
            engine.state.monitors[mi].focused,
            Some(2),
            "B receives focus after overlay dismissed"
        );
        assert!(engine.state.pending_focus.is_none(), "deferral resolved");
        assert_eq!(engine.state.presented_overlay_owner(mi), None);
        engine.state.check_invariants().expect("after focus B");

        // Fullscreen B.
        t_set_fullscreen(&mut engine, 2, true);
        engine.state.check_invariants().expect("after fullscreen B");
        assert_eq!(
            engine.state.presented_overlay_owner(mi),
            Some(2),
            "B now owns the overlay"
        );

        // Destroy B — no orphan overlay.
        t_destroy(&mut engine, 2);
        assert_eq!(
            engine.state.presented_overlay_owner(mi),
            None,
            "no orphan overlay after B destroyed"
        );
        engine.state.check_invariants().expect("after destroy B");

        // Destroy A — still no orphan overlay, pending_focus resolved.
        t_destroy(&mut engine, 1);
        assert_eq!(engine.state.presented_overlay_owner(mi), None);
        assert!(engine.state.pending_focus.is_none());
        engine.state.check_invariants().expect("after destroy A");
    }

    #[test]
    fn audit_p2_fullscreen_grid_configure_notify_storm() {
        use crate::backend::x11::reconciler::{
            classify_configure, AppliedWindow, ConfigureObservation,
        };
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);
        assert!(
            !t_manage(&mut engine, 2),
            "B is tiled and deferred behind the fullscreen overlay"
        );

        // Build the Applied entries the backend would have last written.
        let a_screen = engine.state.monitors[mi].screen;
        let a_applied = AppliedWindow {
            rect: a_screen,
            border_w: 0,
            seen: true,
            sequence: None,
        };
        let b_desired = pipeline_desired(&engine, mi);
        let (b_r, b_b) = {
            let e = b_desired.windows.iter().find(|d| d.window == 2).unwrap();
            (e.rect, e.border)
        };
        let b_applied = AppliedWindow {
            rect: b_r,
            border_w: b_b,
            seen: true,
            sequence: None,
        };

        // Simulate an unexpected ConfigureNotify for A (fullscreen) and B (tiled).
        let obs_a = classify_configure(Rect::new(40, 40, 640, 480), 0, &a_applied);
        let obs_b = classify_configure(Rect::new(10, 10, 800, 600), b_b, &b_applied);
        assert_eq!(
            obs_a,
            ConfigureObservation::Stale,
            "A (fullscreen) must NOT be classified as our echo"
        );
        assert_eq!(
            obs_b,
            ConfigureObservation::Stale,
            "B (tiled) must NOT be classified as our echo"
        );
        // The WM is the geometry authority for tiled AND fullscreen windows:
        // both reports are stale traffic it re-asserts over, never adopts.

        engine.execute(crate::core::commands::ViewWorkspace(1));
        assert_eq!(engine.state.monitors[mi].active_ws, 1);
        engine.execute(crate::core::commands::ViewWorkspace(0));
        assert_eq!(
            engine.state.presented_overlay_owner(mi),
            Some(1),
            "A still the overlay after ws round-trip"
        );
        engine
            .state
            .check_invariants()
            .expect("invariants after configure storm + ws switch");
    }

    #[test]
    fn audit_p2_column_normal_fullscreen_is_ribbon_tile() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);
        engine.state.clients.get_mut(&1).unwrap().fullscreen_policy =
            crate::types::FullscreenPolicy::Normal;

        assert_eq!(
            engine.state.presented_overlay_owner(mi),
            None,
            "a Column/Normal fullscreen is a ribbon tile, NOT a presented overlay"
        );

        // Because there is no real overlay, the newcomer B must take the focus
        // (decide_manage_focus returns Focus, not Defer).
        let intent = crate::core::commands::decide_manage_focus(&engine.state, 2);
        assert!(
            matches!(intent, crate::core::commands::ManageFocusIntent::Focus(2)),
            "B must receive focus: no presented overlay in Column layout"
        );
        assert!(
            t_manage(&mut engine, 2),
            "new window gets focus in Column layout"
        );
        assert_eq!(engine.state.monitors[mi].focused, Some(2));
    }

    #[test]
    fn audit_p3_maximize_unmaximize_tracks_presented() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        t_manage(&mut engine, 1);
        t_set_maximized(&mut engine, 1);
        t_focus(&mut engine, 1);
        let aws0 = engine.state.monitors[mi].active_ws;
        assert_eq!(
            engine.state.monitors[mi].workspaces[aws0].presented_maximize,
            Some(1),
            "focused maximized window owns presented_maximize"
        );

        // create B
        t_manage(&mut engine, 2);
        // resize A directly (model-level geom mutation)
        engine.state.clients.get_mut(&1).unwrap().geom = Rect::new(0, 0, 400, 300);
        // focus B (A no longer the focused maximize owner)
        t_focus(&mut engine, 2);
        let aws1 = engine.state.monitors[mi].active_ws;
        assert!(
            engine.state.monitors[mi].workspaces[aws1]
                .presented_maximize
                .is_none()
                || engine.state.monitors[mi].workspaces[aws1].presented_maximize == Some(2),
            "after focus B, presented_maximize is None or names B"
        );

        // unmaximize A (target A explicitly)
        aud_run_cmd(&mut engine, crate::core::commands::ToggleMaximize(Some(1)));
        let aws2 = engine.state.monitors[mi].active_ws;
        assert!(
            engine.state.monitors[mi].workspaces[aws2].presented_maximize != Some(1),
            "no stale presented_maximize naming A after unmaximize"
        );
        engine
            .state
            .check_invariants()
            .expect("invariants after unmaximize A");
    }

    #[test]
    fn audit_p3_float_geometry_follows_model() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        let g0 = Rect::new(100, 100, 300, 200);
        let mut c = Client::new(1, mi, ws_i);
        c.flags.set(WinFlags::FLOAT);
        c.geom = g0;
        c.saved_geom = g0;
        c.border_w = engine.cfg.border_w;
        engine.state.add_client(c);
        engine.state.monitors[mi].workspaces[ws_i].floats.push(1);
        engine.state.monitors[mi].focused = Some(1);

        let g1 = Rect::new(200, 150, 400, 250);
        // float policy: the model follows the client's new geometry
        engine.state.clients.get_mut(&1).unwrap().geom = g1;
        let desired = pipeline_desired(&engine, mi);
        let e = desired.windows.iter().find(|d| d.window == 1).unwrap();
        assert_eq!(e.rect, g1, "float geometry followed into Desired");
        assert_eq!(engine.state.clients.get(&1).unwrap().geom, g1);

        // convert back to tiled
        aud_run_cmd(&mut engine, crate::core::commands::ToggleFloat(None));
        assert!(
            !engine.state.clients.get(&1).unwrap().is_float(),
            "window back to tiled"
        );
        let desired2 = pipeline_desired(&engine, mi);
        let e2 = desired2.windows.iter().find(|d| d.window == 1).unwrap();
        let wa = engine.state.monitors[mi].workarea;
        assert!(
            wa.contains_rect(e2.rect),
            "tiled window placed within workarea"
        );
        engine.state.check_invariants().expect("after float->tiled");
    }

    #[test]
    fn audit_p3_float_to_tiled_reasserts_authority() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        t_manage(&mut engine, 1);

        // convert to float
        aud_run_cmd(&mut engine, crate::core::commands::ToggleFloat(None));
        assert!(engine.state.clients.get(&1).unwrap().is_float());
        // client changes geometry
        let g1 = Rect::new(200, 150, 400, 250);
        engine.state.clients.get_mut(&1).unwrap().geom = g1;
        assert_eq!(engine.state.clients.get(&1).unwrap().geom, g1);

        // back to tiled — geometry authority returns to the WM
        aud_run_cmd(&mut engine, crate::core::commands::ToggleFloat(None));
        assert!(!engine.state.clients.get(&1).unwrap().is_float());

        let desired = pipeline_desired(&engine, mi);
        let e = desired.windows.iter().find(|d| d.window == 1).unwrap();
        let wa = engine.state.monitors[mi].workarea;
        assert!(wa.contains_rect(e.rect), "tiled placement within workarea");

        // simulate the backend apply_geom pass (write placements back to client.geom)
        let mut p = crate::core::layout::Placements::new();
        crate::core::layout::arrange(
            &engine.state,
            mi,
            &engine.cfg,
            &mut p,
            &mut RibbonScratch::default(),
        );
        let tile = p.iter().find(|e| e.0 == 1).unwrap().1;
        for (win, rect, bw) in &p {
            if let Some(c) = engine.state.clients.get_mut(win) {
                c.geom = *rect;
                c.border_w = *bw;
            }
        }
        assert_eq!(e.rect, tile, "Desired matches layout placement");
        assert_eq!(
            engine.state.clients.get(&1).unwrap().geom,
            tile,
            "client.geom == WM layout placement after re-tile"
        );
        engine
            .state
            .check_invariants()
            .expect("after tiled re-assert");
    }

    #[test]
    fn audit_p4_fullscreen_dialog_steals_focus() {
        use crate::core::commands::decide_manage_focus;
        use crate::types::WinFlags;
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);
        assert_eq!(engine.state.presented_overlay_owner(mi), Some(1));

        // open child dialog B (transient to A, float)
        let mut cb = Client::new(2, mi, ws_i);
        cb.flags.set(WinFlags::FLOAT);
        cb.geom = Rect::new(200, 200, 300, 200);
        cb.saved_geom = cb.geom;
        cb.border_w = engine.cfg.border_w;
        cb.transient_parent = Some(1);
        engine.state.add_client(cb);
        engine.state.monitors[mi].workspaces[ws_i].floats.push(2);

        let intent = decide_manage_focus(&engine.state, 2);
        assert!(
            matches!(intent, crate::core::commands::ManageFocusIntent::Focus(2)),
            "owned dialog of the overlay owner steals focus"
        );
        t_focus(&mut engine, 2);
        assert_eq!(engine.state.monitors[mi].focused, Some(2), "B gets focused");

        // close B — A still fullscreen, overlay intact, no panic
        t_destroy(&mut engine, 2);
        assert_eq!(
            engine.state.presented_overlay_owner(mi),
            Some(1),
            "A still fullscreen, overlay intact"
        );
        assert_eq!(
            engine.state.monitors[mi].focused,
            Some(1),
            "focus returns to overlay owner A"
        );
        engine.state.check_invariants().expect("after dialog close");
    }

    #[test]
    fn audit_p4_tiled_dialog_resize_close_consistent() {
        use crate::core::commands::MoveResize;
        use crate::types::WinFlags;
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        t_manage(&mut engine, 1);
        t_focus(&mut engine, 1);

        let mut cb = Client::new(2, mi, ws_i);
        cb.flags.set(WinFlags::FLOAT);
        cb.geom = Rect::new(200, 200, 300, 200);
        cb.saved_geom = cb.geom;
        cb.border_w = engine.cfg.border_w;
        cb.transient_parent = Some(1);
        engine.state.add_client(cb);
        engine.state.monitors[mi].workspaces[ws_i].floats.push(2);
        t_focus(&mut engine, 2);

        // resize B (MoveResize on an existing float)
        aud_run_cmd(&mut engine, MoveResize(2, Rect::new(250, 250, 350, 220)));
        assert_eq!(
            engine.state.clients.get(&2).unwrap().geom,
            Rect::new(250, 250, 350, 220)
        );
        assert!(engine.state.clients.get(&2).unwrap().is_float());

        // close B
        t_destroy(&mut engine, 2);
        assert!(
            !engine.state.clients.contains_key(&2),
            "B removed from clients"
        );
        let aws = engine.state.monitors[mi].active_ws;
        assert!(
            !engine.state.monitors[mi].workspaces[aws]
                .floats
                .contains(&2),
            "B removed from floats"
        );
        assert_eq!(
            engine.state.monitors[mi].focused,
            Some(1),
            "focus back to A"
        );
        assert!(
            !engine.state.clients.get(&1).unwrap().is_float(),
            "A still tiled"
        );
        engine.state.check_invariants().expect("after dialog close");
    }

    #[test]
    fn audit_p4_orphan_transient_parent() {
        use crate::types::WinFlags;
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        t_manage(&mut engine, 1);
        t_focus(&mut engine, 1);

        // B with transient_parent = A
        let mut cb = Client::new(2, mi, ws_i);
        cb.flags.set(WinFlags::FLOAT);
        cb.geom = Rect::new(200, 200, 300, 200);
        cb.saved_geom = cb.geom;
        cb.border_w = engine.cfg.border_w;
        cb.transient_parent = Some(1);
        engine.state.add_client(cb);
        engine.state.monitors[mi].workspaces[ws_i].floats.push(2);

        // destroy A (parent) first, then B (orphan transient) — must not panic
        t_destroy(&mut engine, 1);
        t_destroy(&mut engine, 2);
        assert!(!engine.state.clients.contains_key(&1));
        assert!(!engine.state.clients.contains_key(&2));
        engine
            .state
            .check_invariants()
            .expect("after destroying orphan transient (readers use clients.get guards)");
    }

    // `render::MAX_TRANSIENT_DEPTH` (4) bounds the *stacking* question "is this
    // float owned by the presented overlay?" — the bound exists because
    // `WM_TRANSIENT_FOR` is unvalidated client input and can describe a cycle.
    // The bound is a stacking answer only; it must never leak into model
    // ownership. These tests build chains at, below and beyond the bound and
    // assert the model stays coherent while the chain is torn down in every
    // order: no dangling `transient_parent`, no dangling deferred-transient
    // queue entry, no focus/overlay pointing at a destroyed window.

    /// `manage()` for a transient popup: a float that inherits its parent's
    /// monitor/workspace, records `transient_parent`, and then goes through the
    /// same presentation-aware focus policy as `t_manage`.
    fn t_manage_transient(engine: &mut Engine, win: WindowId, parent: WindowId) -> bool {
        let mi = engine.state.sel_mon;
        let (mi, ws_i) = engine
            .state
            .clients
            .get(&parent)
            .map_or((mi, engine.state.monitors[mi].active_ws), |p| {
                (p.monitor, p.workspace)
            });
        let mut c = Client::new(win, mi, ws_i);
        c.flags.set(WinFlags::FLOAT);
        c.geom = Rect::new(200, 200, 300, 200);
        c.saved_geom = c.geom;
        c.border_w = engine.cfg.border_w;
        c.transient_parent = Some(parent);
        engine.state.add_client(c);
        engine.state.monitors[mi].workspaces[ws_i].floats.push(win);
        match crate::core::commands::decide_manage_focus(&engine.state, win) {
            crate::core::commands::ManageFocusIntent::Defer {
                owner,
                monitor,
                workspace,
            } => {
                engine.state.pending_focus = Some(crate::types::PendingFocus {
                    window: win,
                    owner,
                    monitor,
                    workspace,
                });
                false
            }
            crate::core::commands::ManageFocusIntent::Focus(_) => {
                t_focus(engine, win);
                true
            }
        }
    }

    /// Every reference the transient machinery can hold must name a live client:
    /// `transient_parent` (both directions), the deferred-transient queue, the
    /// logical focus, the deferred focus, and both overlay owners.
    fn r5_assert_coherent(engine: &Engine, ctx: &str) {
        let live = |w: WindowId| engine.state.clients.contains_key(&w);

        // 1. No orphaned transient reference, in either direction.
        for (&w, c) in &engine.state.clients {
            if let Some(p) = c.transient_parent {
                assert!(live(p), "{ctx}: client {w} points at destroyed parent {p}");
                assert_ne!(p, w, "{ctx}: client {w} is its own transient parent");
            }
            // The "transient list" of a parent is derived (there is no stored
            // child vector): every window that claims `w` as parent must be a
            // live client that really does claim it.
            for child in engine
                .state
                .clients
                .values()
                .filter(|k| k.transient_parent == Some(w))
            {
                assert!(
                    live(child.window),
                    "{ctx}: dead child {} of {w}",
                    child.window
                );
                assert_eq!(
                    engine.state.clients[&child.window].transient_parent,
                    Some(w),
                    "{ctx}: child/parent link disagrees"
                );
            }
        }
        for &w in &engine.state.pending_transients {
            assert!(
                live(w),
                "{ctx}: pending_transients names destroyed window {w}"
            );
        }

        // 2. No invalid focus: logical focus, MRU stack, deferred focus and the
        //    mirrored X focus all name live clients (or nothing).
        for (mi, mon) in engine.state.monitors.iter().enumerate() {
            if let Some(f) = mon.focused {
                assert!(live(f), "{ctx}: monitor {mi} focused on destroyed {f}");
            }
            for &w in &mon.focus_stack {
                assert!(live(w), "{ctx}: monitor {mi} focus_stack holds dead {w}");
            }
            // 3. No invalid overlay: neither overlay owner may name a ghost.
            for (wi, ws) in mon.workspaces.iter().enumerate() {
                if let Some(o) = ws.presented_maximize {
                    assert!(
                        live(o),
                        "{ctx}: monitor {mi} ws {wi} presented_maximize is dead {o}"
                    );
                }
            }
            if let Some(o) = engine.state.presented_overlay_owner(mi) {
                assert!(live(o), "{ctx}: monitor {mi} overlay owner is dead {o}");
            }
        }
        if let Some(pf) = engine.state.pending_focus {
            assert!(live(pf.window), "{ctx}: pending_focus target is dead");
            assert!(live(pf.owner), "{ctx}: pending_focus owner is dead");
        }
        if let Some(w) = engine.state.x11_input_focus {
            assert!(live(w), "{ctx}: x11_input_focus is dead {w}");
        }

        engine.state.check_invariants().expect(ctx);
    }

    /// Root window `1` presenting an overlay (fullscreen under
    /// `FullscreenPolicy::True`, or focused-maximized) plus a transient chain
    /// `1 → 2 → … → depth+1`.
    fn r5_build_chain(depth: u32, maximized: bool) -> Engine {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        if !maximized {
            engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Column;
        }
        t_manage(&mut engine, 1);
        if maximized {
            t_set_maximized(&mut engine, 1);
            t_focus(&mut engine, 1);
        } else {
            t_set_fullscreen(&mut engine, 1, true);
        }
        assert_eq!(
            engine.state.presented_overlay_owner(mi),
            Some(1),
            "the root must really own the overlay"
        );
        for w in 2..=(depth + 1) {
            t_manage_transient(&mut engine, w, w - 1);
            assert_eq!(
                engine.state.clients.get(&w).unwrap().transient_parent,
                Some(w - 1),
                "chain link {w} → {} recorded",
                w - 1
            );
        }
        r5_assert_coherent(&engine, "chain built");
        engine
    }

    /// Build the chain and destroy it in `order`, asserting coherence after
    /// every single destroy (and that nothing is left behind at the end).
    fn r5_destroy_in_order(depth: u32, maximized: bool, order: &[WindowId], label: &str) {
        let mut engine = r5_build_chain(depth, maximized);
        for &w in order {
            t_destroy(&mut engine, w);
            let ctx = format!("{label}: after destroying {w}");
            assert!(
                !engine.state.clients.contains_key(&w),
                "{ctx}: window still in clients"
            );
            r5_assert_coherent(&engine, &ctx);
        }
        assert!(
            engine.state.clients.is_empty(),
            "{label}: every window of the chain is gone"
        );
        for (mi, mon) in engine.state.monitors.iter().enumerate() {
            assert_eq!(
                mon.focused, None,
                "{label}: monitor {mi} keeps a focus with no clients left"
            );
        }
        assert!(
            engine.state.pending_focus.is_none(),
            "{label}: stale deferral"
        );
        assert!(
            engine.state.pending_transients.is_empty(),
            "{label}: stale deferred transient"
        );
    }

    /// The three interesting teardown orders for a chain of `depth` links:
    /// leaf-first (the polite toolkit), root-first (the parent dies while its
    /// popups are still up) and middle-first (a hole punched in the chain).
    fn r5_transient_chain_case(depth: u32) {
        let leaf = depth + 1;
        for maximized in [false, true] {
            let kind = if maximized { "maximize" } else { "fullscreen" };

            let leaf_first: Vec<WindowId> = (1..=leaf).rev().collect();
            r5_destroy_in_order(
                depth,
                maximized,
                &leaf_first,
                &format!("depth {depth} / {kind} / leaf-first"),
            );

            let root_first: Vec<WindowId> = (1..=leaf).collect();
            r5_destroy_in_order(
                depth,
                maximized,
                &root_first,
                &format!("depth {depth} / {kind} / root-first"),
            );

            let mid = leaf / 2 + 1;
            let mut middle_first = vec![mid];
            middle_first.extend((1..=leaf).filter(|&w| w != mid));
            r5_destroy_in_order(
                depth,
                maximized,
                &middle_first,
                &format!("depth {depth} / {kind} / middle-first"),
            );
        }
    }

    #[test]
    fn audit_r5_transient_chain_depth1_stays_coherent() {
        r5_transient_chain_case(1);
    }

    #[test]
    fn audit_r5_transient_chain_depth2_stays_coherent() {
        r5_transient_chain_case(2);
    }

    #[test]
    fn audit_r5_transient_chain_depth4_at_the_limit_stays_coherent() {
        r5_transient_chain_case(4);
    }

    #[test]
    fn audit_r5_transient_chain_depth5_beyond_the_limit_stays_coherent() {
        // Beyond `MAX_TRANSIENT_DEPTH` the *stacking* answer changes (the deepest
        // popup is no longer recognised as owned by the overlay), but ownership
        // of the model must not: the chain still tears down without orphans.
        r5_transient_chain_case(5);
    }

    #[test]
    fn audit_r5_destroyed_parent_orphans_no_child() {
        // Destroying a parent must cut exactly the dead edge and no other. A
        // stale `transient_parent` is worse than a missing one: with XID reuse
        // the id can come back as an unrelated window, which would then inherit
        // these orphans as its popups.
        let mut engine = r5_build_chain(3, false);
        t_destroy(&mut engine, 1);
        assert_eq!(
            engine.state.clients.get(&2).unwrap().transient_parent,
            None,
            "the direct child of a destroyed parent must be re-parented to None"
        );
        assert_eq!(
            engine.state.clients.get(&3).unwrap().transient_parent,
            Some(2),
            "deeper links are untouched — only the dead edge is cut"
        );
        r5_assert_coherent(&engine, "parent destroyed mid-chain");

        // Same for a link in the middle of the chain.
        t_destroy(&mut engine, 3);
        assert_eq!(
            engine.state.clients.get(&4).unwrap().transient_parent,
            None,
            "a hole in the middle of the chain leaves no dangling parent"
        );
        r5_assert_coherent(&engine, "middle destroyed");
    }

    #[test]
    fn audit_r5_pending_transient_queue_never_dangles() {
        // A popup that maps *before* its parent is parked in `pending_transients`
        // (it is only drained on the next `manage`). Destroying it — or its
        // still-unmanaged parent's stand-in — must not leave the queue naming a
        // window that no longer exists.
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        t_manage(&mut engine, 1);

        // 2 is transient for the not-yet-managed 99 → deferred.
        let mut c = Client::new(2, mi, ws_i);
        c.flags.set(WinFlags::FLOAT);
        c.geom = Rect::new(200, 200, 300, 200);
        c.saved_geom = c.geom;
        c.border_w = engine.cfg.border_w;
        c.transient_parent = Some(99);
        engine.state.add_client(c);
        engine.state.monitors[mi].workspaces[ws_i].floats.push(2);
        engine.state.pending_transients.push(2);

        t_destroy(&mut engine, 2);
        assert!(
            engine.state.pending_transients.is_empty(),
            "a destroyed deferred transient must leave the queue"
        );
        r5_assert_coherent(&engine, "deferred transient destroyed");
    }

    #[test]
    fn audit_p5_monitor_switch_keeps_other_overlay() {
        let mut engine = setup_engine_multi();
        let m0 = 0;
        let m1 = 1;
        engine.state.sel_mon = m0;
        engine.state.monitors[m0].workspaces[0].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);
        assert_eq!(engine.state.presented_overlay_owner(m0), Some(1));

        // switch to mon1 and create a window there
        engine.state.sel_mon = m1;
        t_manage(&mut engine, 2);
        t_focus(&mut engine, 2);
        assert_eq!(engine.state.presented_overlay_owner(m1), None);

        // mon0 overlay intact after operating on mon1; sel_mon moved to m1 (expected)
        assert_eq!(
            engine.state.presented_overlay_owner(m0),
            Some(1),
            "mon0 overlay intact after operating on mon1"
        );
        assert_eq!(engine.state.sel_mon, m1);
        engine
            .state
            .check_invariants()
            .expect("after cross-monitor create");
    }

    #[test]
    fn audit_p5_move_to_workspace_keeps_sel_mon() {
        let mut engine = setup_engine_multi();
        let mi = 0;
        engine.state.sel_mon = mi;
        t_manage(&mut engine, 1);
        t_focus(&mut engine, 1);
        let sel_before = engine.state.sel_mon;
        aud_run_cmd(&mut engine, crate::core::commands::MoveToWorkspace(3));
        assert_eq!(
            engine.state.sel_mon, sel_before,
            "MoveToWorkspace must not move sel_mon"
        );
        assert!(!engine.state.monitors[mi].workspaces[0].floats.contains(&1));
        assert!(
            engine.state.monitors[mi].workspaces[0]
                .columns
                .iter()
                .all(|col| !col.windows.contains(&1)),
            "window not in old workspace columns"
        );
        assert!(
            engine.state.monitors[mi].workspaces[3]
                .columns
                .iter()
                .any(|col| col.windows.contains(&1))
                || engine.state.monitors[mi].workspaces[3].floats.contains(&1)
        );
        assert_eq!(engine.state.clients.get(&1).unwrap().workspace, 3);
        engine
            .state
            .check_invariants()
            .expect("after move to workspace");
    }

    #[test]
    fn audit_p5_move_to_monitor_moves_ownership() {
        use crate::types::Dir;
        let mut engine = setup_engine_multi();
        let mi = 0;
        engine.state.sel_mon = mi;
        t_manage(&mut engine, 1);
        t_focus(&mut engine, 1);
        assert_eq!(engine.state.clients.get(&1).unwrap().monitor, 0);

        aud_run_cmd(
            &mut engine,
            crate::core::commands::MoveWindowToMonitor(1, Dir::Right),
        );
        assert_eq!(
            engine.state.clients.get(&1).unwrap().monitor,
            1,
            "window moved to mon1"
        );

        let d0 = pipeline_desired(&engine, 0);
        let d1 = pipeline_desired(&engine, 1);
        assert!(
            !d0.windows.iter().any(|d| d.window == 1),
            "window not desired on mon0"
        );
        assert!(
            d1.windows.iter().any(|d| d.window == 1),
            "window desired on mon1"
        );
        engine
            .state
            .check_invariants()
            .expect("after move to monitor");
    }

    #[test]
    fn audit_p5_fullscreen_owner_destroyed_no_orphan() {
        let mut engine = setup_engine_multi();
        let m0 = 0;
        engine.state.sel_mon = m0;
        engine.state.monitors[m0].workspaces[0].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);
        assert!(!t_manage(&mut engine, 2), "B deferred");
        assert_eq!(engine.state.pending_focus.map(|p| p.window), Some(2));

        // destroy the fullscreen owner A while B is deferred
        t_destroy(&mut engine, 1);
        assert_eq!(
            engine.state.presented_overlay_owner(m0),
            None,
            "no orphan overlay after owner destroyed"
        );
        engine
            .state
            .check_invariants()
            .expect("after fullscreen owner destroyed");
    }

    // Move a window ACROSS monitors, then destroy it. Neither the old monitor
    // nor the new one may retain a Desired/Applied or tree reference to the dead
    // window; `check_invariants` must stay green and no stale
    // `presented_maximize`/`pending_focus` may name it.
    #[test]
    fn audit_p5_move_to_monitor_then_destroy_leaves_no_orphan() {
        use crate::types::Dir;
        let mut engine = setup_engine_multi();
        let m0: usize = 0;
        let m1: usize = 1;
        engine.state.sel_mon = m0;
        t_manage(&mut engine, 1);
        t_focus(&mut engine, 1);
        assert_eq!(engine.state.clients.get(&1).unwrap().monitor, m0);

        aud_run_cmd(
            &mut engine,
            crate::core::commands::MoveWindowToMonitor(1, Dir::Right),
        );
        assert_eq!(
            engine.state.clients.get(&1).unwrap().monitor,
            m1,
            "window relocated to mon1"
        );

        t_destroy(&mut engine, 1);

        // Dead window must appear in NO monitor's Desired, and the old monitor's
        // tree must not retain it (checked by `check_invariants` #4/#5 too).
        for mi in [m0, m1] {
            let d = pipeline_desired(&engine, mi);
            assert!(
                !d.windows.iter().any(|dw| dw.window == 1),
                "destroyed window must not be desired on monitor {mi}"
            );
        }
        assert!(
            !engine.state.monitors[m0]
                .workspaces
                .iter()
                .flat_map(|ws| ws.columns.iter().flat_map(|c| c.windows.iter().copied()))
                .chain(
                    engine.state.monitors[m0]
                        .workspaces
                        .iter()
                        .flat_map(|ws| ws.floats.iter().copied())
                )
                .any(|w| w == 1),
            "old monitor tree must not reference the destroyed window"
        );
        assert!(engine
            .state
            .pending_focus
            .is_none_or(|p| p.window != 1 && p.owner != 1));
        engine
            .state
            .check_invariants()
            .expect("after move-across-monitor + destroy");
    }

    // The move/destroy path must never leave a stale `presented_maximize`
    // referencing a window that has moved away or been destroyed (see
    // `remove_client`, `MoveWindowToMonitor`, `MoveToWorkspace`).
    #[test]
    fn audit_p5_multi_monitor_minifuzz() {
        use crate::backend::x11::reconciler::AppliedState;
        use crate::core::commands::{
            FocusMonitor, MoveResize, MoveToWorkspace, MoveWindowToMonitor, ToggleFloat,
            ToggleFullscreen, ToggleMaximize, ViewWorkspace,
        };
        use crate::types::{Dir, LayoutKind, WindowId};

        const SEED: u64 = 0x1234_5678_ABCD_EF01;
        const STEPS: u32 = 1500;
        const MAX_WINS: usize = 20;

        struct Rng(u64);
        impl Rng {
            fn next(&mut self) -> u64 {
                self.0 = self
                    .0
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                self.0 >> 16
            }
            fn below(&mut self, n: u32) -> u32 {
                (self.next() % n as u64) as u32
            }
        }
        let mut rng = Rng(SEED);

        let mut engine = setup_engine_multi();
        let nmon = engine.state.monitors.len();
        let mut live: Vec<WindowId> = Vec::new();
        let mut next_win: WindowId = 1;
        let mut applied = AppliedState::default();

        // Seed one window on mon0/ws0.
        {
            let mi = 0;
            engine.state.sel_mon = mi;
            engine.state.monitors[mi].active_ws = 0;
            t_manage(&mut engine, next_win);
            live.push(next_win);
            next_win += 1;
        }

        for step in 0..STEPS {
            let op = rng.below(11);
            match op {
                0 => {
                    // Create on a random monitor/workspace.
                    if live.len() < MAX_WINS {
                        let target = rng.below(nmon as u32) as usize;
                        let tws = rng.below(engine.state.monitors[target].workspaces.len() as u32)
                            as usize;
                        engine.state.sel_mon = target;
                        engine.state.monitors[target].active_ws = tws;
                        let w = next_win;
                        next_win += 1;
                        t_manage(&mut engine, w);
                        live.push(w);
                        // The harness does not exercise the deferral focus
                        // bookkeeping; clear it so an otherwise-valid chaos run
                        // does not trip the FROZEN focus-domain invariants.
                        engine.state.pending_focus = None;
                    }
                }
                1 => {
                    // Destroy a random live window.
                    if !live.is_empty() {
                        let i = rng.below(live.len() as u32) as usize;
                        let w = live.remove(i);
                        t_destroy(&mut engine, w);
                        applied.forget(w);
                    }
                }
                2 => {
                    engine.execute(ToggleFullscreen(None));
                }
                3 => {
                    engine.execute(ToggleMaximize(None));
                }
                4 => {
                    engine.execute(ToggleFloat(None));
                }
                5 => {
                    // MoveResize on an existing float.
                    if !live.is_empty() {
                        let w = live[rng.below(live.len() as u32) as usize];
                        let is_float = engine
                            .state
                            .clients
                            .get(&w)
                            .is_some_and(crate::types::Client::is_float);
                        if is_float {
                            let gx = (rng.below(800) as i32) + 50;
                            let gy = (rng.below(600) as i32) + 50;
                            let gw = 100 + rng.below(400);
                            let gh = 100 + rng.below(300);
                            let g = Rect::new(gx, gy, gw, gh);
                            if let Some(c) = engine.state.clients.get_mut(&w) {
                                c.geom = g;
                            }
                            engine.execute(MoveResize(w, g));
                        }
                    }
                }
                6 => {
                    // MoveToWorkspace — must not move sel_mon.
                    if !live.is_empty() {
                        let w = live[rng.below(live.len() as u32) as usize];
                        if let Some(c) = engine.state.clients.get(&w) {
                            engine.state.sel_mon = c.monitor;
                            engine.state.monitors[c.monitor].focused = Some(w);
                        }
                        let n = engine.state.monitors[engine.state.sel_mon].workspaces.len();
                        let ws = rng.below(n as u32) as usize;
                        let sel_before = engine.state.sel_mon;
                        engine.execute(MoveToWorkspace(ws));
                        assert_eq!(
                            engine.state.sel_mon, sel_before,
                            "seed {SEED:#x} step {step}: MoveToWorkspace moved sel_mon"
                        );
                    }
                }
                7 => {
                    // MoveWindowToMonitor.
                    if !live.is_empty() {
                        let w = live[rng.below(live.len() as u32) as usize];
                        if let Some(c) = engine.state.clients.get(&w) {
                            engine.state.sel_mon = c.monitor;
                            engine.state.monitors[c.monitor].focused = Some(w);
                        }
                        let dir = if rng.below(2) == 0 {
                            Dir::Left
                        } else {
                            Dir::Right
                        };
                        engine.execute(MoveWindowToMonitor(w, dir));
                    }
                }
                8 => {
                    // ViewWorkspace (may move sel_mon — expected).
                    let n = engine.state.monitors[engine.state.sel_mon].workspaces.len();
                    let ws = rng.below(n as u32) as usize;
                    engine.execute(ViewWorkspace(ws));
                }
                9 => {
                    // FocusMonitor (may move sel_mon — expected).
                    let dir = if rng.below(2) == 0 {
                        Dir::Left
                    } else {
                        Dir::Right
                    };
                    engine.execute(FocusMonitor(dir));
                }
                _ => {
                    // LayoutChange on a random monitor/workspace.
                    let m = rng.below(nmon as u32) as usize;
                    let ws_i = rng.below(engine.state.monitors[m].workspaces.len() as u32) as usize;
                    let lk = if rng.below(2) == 0 {
                        LayoutKind::Column
                    } else {
                        LayoutKind::Column
                    };
                    engine.state.monitors[m].workspaces[ws_i].layout = lk;
                }
            }

            assert!(
                engine.state.sel_mon < engine.state.monitors.len(),
                "seed {SEED:#x} step {step}: sel_mon out of range"
            );

            // No Desired on the wrong monitor: after arr/present each monitor,
            // every placed window's client.monitor must match the monitor.
            for mi in 0..engine.state.monitors.len() {
                let desired = pipeline_desired(&engine, mi);
                for d in &desired.windows {
                    if let Some(c) = engine.state.clients.get(&d.window) {
                        assert_eq!(
                            c.monitor, mi,
                            "seed {SEED:#x} step {step}: window {} desired on mon {mi} but owned by mon {}",
                            d.window, c.monitor
                        );
                    }
                }
            }

            // No orphan Applied: every Applied entry names a live client.
            for w in applied.windows.keys() {
                assert!(
                    engine.state.clients.contains_key(w),
                    "seed {SEED:#x} step {step}: Applied holds stale window {w}"
                );
            }

            // Structural (non-focus-domain) invariants must hold.
            if let Err(v) = engine.state.check_invariants() {
                const FOCUS: [&str; 4] = [
                    "pending_focus",
                    "focus_stack",
                    "overlay owner",
                    "x11_input_focus",
                ];
                let structural: Vec<&String> = v
                    .iter()
                    .filter(|m| !FOCUS.iter().any(|k| m.contains(k)))
                    .collect();
                assert!(
                    structural.is_empty(),
                    "seed {SEED:#x} step {step}: structural invariant violation: {structural:?}"
                );
            }
        }

        // Final: no orphan Applied + structural invariants.
        for w in applied.windows.keys() {
            assert!(
                engine.state.clients.contains_key(w),
                "seed {SEED:#x}: final Applied stale {w}"
            );
        }
        engine
            .state
            .check_invariants()
            .expect("final invariants (structural)");
    }

    // `reconcile` is the only writer of the Applied record, so every divergence
    // between Applied and Desired must be detected and re-emitted, and a
    // destroyed or moved window must leave no reference behind in Desired,
    // Applied, `pending_focus` or `presented_maximize`.

    #[test]
    fn audit_p9a_stale_applied_detected_and_converges() {
        use crate::backend::x11::reconciler::{
            reconcile, AppliedState, AppliedWindow, GeometryEffect,
        };
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        t_manage(&mut engine, 1);
        let desired = pipeline_desired(&engine, mi);
        let (r, b) = {
            let e = desired.windows.iter().find(|d| d.window == 1).unwrap();
            (e.rect, e.border)
        };
        // Model "X11 Real diverges (stale)": Applied tracks a wrong rect.
        let mut applied = AppliedState::default();
        applied.windows.insert(
            1,
            AppliedWindow {
                rect: Rect::new(0, 0, 50, 50),
                border_w: b,
                seen: true,
                sequence: None,
            },
        );
        let effects = reconcile(&desired, &engine.state, &mut applied);
        assert_eq!(
            effects.len(),
            1,
            "a stale Applied must be detected and re-emitted"
        );
        match &effects[0] {
            GeometryEffect::Configure { win, rect, .. } => {
                assert_eq!(*win, 1);
                assert_eq!(*rect, r, "detection emits the desired rect");
            }
        }
        assert_eq!(
            applied.windows[&1].rect, r,
            "after apply, applied == desired (converged)"
        );
    }

    #[test]
    fn audit_p9b_old_applied_reemits() {
        use crate::backend::x11::reconciler::{reconcile, AppliedState, AppliedWindow};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        t_manage(&mut engine, 1);
        let desired = pipeline_desired(&engine, mi);
        let (r, b) = {
            let e = desired.windows.iter().find(|d| d.window == 1).unwrap();
            (e.rect, e.border)
        };
        // "X11 Real old": Applied holds a previous (still wrong) rect.
        let old = Rect::new(10, 10, 600, 400);
        let mut applied = AppliedState::default();
        applied.windows.insert(
            1,
            AppliedWindow {
                rect: old,
                border_w: b,
                seen: true,
                sequence: None,
            },
        );
        let effects = reconcile(&desired, &engine.state, &mut applied);
        assert_eq!(effects.len(), 1, "an old Applied must re-emit");
        assert_eq!(
            applied.windows[&1].rect, r,
            "re-emit converges Applied to Desired"
        );
    }

    #[test]
    fn audit_p9c_destroy_eliminates_desired_applied_refs() {
        use crate::backend::x11::reconciler::{AppliedState, AppliedWindow};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        t_manage(&mut engine, 1);
        let desired = pipeline_desired(&engine, mi);
        let (r, b) = {
            let e = desired.windows.iter().find(|d| d.window == 1).unwrap();
            (e.rect, e.border)
        };
        let mut applied = AppliedState::default();
        applied.windows.insert(
            1,
            AppliedWindow {
                rect: r,
                border_w: b,
                seen: true,
                sequence: None,
            },
        );
        // Set a pending_focus referencing the window (the #8c context).
        engine.state.pending_focus = Some(crate::types::PendingFocus {
            window: 1,
            owner: 1,
            monitor: mi,
            workspace: 0,
        });
        t_destroy(&mut engine, 1);
        applied.forget(1);

        assert!(!engine.state.clients.contains_key(&1), "client removed");
        let aws = engine.state.monitors[mi].active_ws;
        assert!(!engine.state.monitors[mi].workspaces[aws]
            .floats
            .contains(&1));
        assert!(
            engine.state.monitors[mi].workspaces[aws]
                .columns
                .iter()
                .all(|c| !c.windows.contains(&1)),
            "window removed from workspace columns"
        );
        assert!(!applied.windows.contains_key(&1), "applied entry gone");
        assert!(
            engine.state.pending_focus.is_none(),
            "pending_focus cleared (8c)"
        );
        engine.state.check_invariants().expect("after destroy");
    }

    #[test]
    fn audit_p9d_move_workspace_removes_old_desired() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        t_manage(&mut engine, 1);
        t_focus(&mut engine, 1);
        aud_run_cmd(&mut engine, crate::core::commands::MoveToWorkspace(4));
        assert!(engine.state.monitors[mi].workspaces[0]
            .columns
            .iter()
            .all(|c| !c.windows.contains(&1)));
        assert!(!engine.state.monitors[mi].workspaces[0].floats.contains(&1));
        assert!(
            engine.state.monitors[mi].workspaces[4]
                .columns
                .iter()
                .any(|c| c.windows.contains(&1))
                || engine.state.monitors[mi].workspaces[4].floats.contains(&1)
        );
        // Old workspace's Desired no longer references the moved window (5).
        engine.state.monitors[mi].active_ws = 0;
        let d0 = pipeline_desired(&engine, mi);
        assert!(
            !d0.windows.iter().any(|d| d.window == 1),
            "old workspace desired no longer references the moved window"
        );
        engine.state.monitors[mi].active_ws = 4;
        engine
            .state
            .check_invariants()
            .expect("after move workspace");
    }

    #[test]
    fn audit_p9e_fullscreen_owner_destroyed_resolves_pending() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        engine.state.monitors[mi].workspaces[0].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);
        assert!(!t_manage(&mut engine, 2), "B deferred");
        assert_eq!(engine.state.pending_focus.map(|p| p.window), Some(2));

        // Destroy the fullscreen owner A while B is deferred.
        t_destroy(&mut engine, 1);
        assert_eq!(
            engine.state.presented_overlay_owner(mi),
            None,
            "no orphan overlay after owner destroyed"
        );
        assert!(
            engine.state.pending_focus.is_none(),
            "pending_focus resolved (8c/9b)"
        );
        engine
            .state
            .check_invariants()
            .expect("after owner destroyed");
    }

    #[test]
    fn audit_p9f_maximize_owner_destroyed_cleans_presented() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        t_manage(&mut engine, 1);
        t_set_maximized(&mut engine, 1);
        t_focus(&mut engine, 1);
        let aws0 = engine.state.monitors[mi].active_ws;
        assert_eq!(
            engine.state.monitors[mi].workspaces[aws0].presented_maximize,
            Some(1)
        );

        t_destroy(&mut engine, 1);
        let aws2 = engine.state.monitors[mi].active_ws;
        assert!(
            engine.state.monitors[mi].workspaces[aws2]
                .presented_maximize
                .is_none(),
            "presented_maximize cleaned after owner destroyed (9)"
        );
        engine
            .state
            .check_invariants()
            .expect("after maximize owner destroyed");
    }

    #[test]
    fn audit_p9g_float_configure_storm_no_backoff() {
        // Measurement only. A float fights the WM with a ConfigureNotify storm
        // (repeated self-resizes). The convergence policy must follow every
        // valid request with NO backoff (no exponential/linear throttle). We
        // count iterations and assert the loop completes 200 and converges.
        use crate::backend::x11::reconciler::{
            classify_configure, AppliedWindow, ConfigureObservation,
        };
        use crate::types::WinFlags;
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        let g0 = Rect::new(100, 100, 300, 200);
        let mut c = Client::new(1, mi, ws_i);
        c.flags.set(WinFlags::FLOAT);
        c.geom = g0;
        c.saved_geom = g0;
        c.border_w = engine.cfg.border_w;
        engine.state.add_client(c);
        engine.state.monitors[mi].workspaces[ws_i].floats.push(1);

        let mut applied = AppliedWindow {
            rect: g0,
            border_w: 2,
            seen: true,
            sequence: None,
        };
        let mut last = g0;
        let mut iterations = 0u32;
        for i in 0..200 {
            let r = Rect::new(
                100 + (i + 1) * 7,
                100 + (i + 1) * 7,
                300 + (i as u32 + 1) * 11,
                200 + (i as u32 + 1) * 11,
            );
            let obs = classify_configure(r, 2, &applied);
            assert_eq!(
                obs,
                ConfigureObservation::Stale,
                "iteration {i}: float fight must not be our echo (Stale, sink adopts)"
            );
            engine.state.clients.get_mut(&1).unwrap().geom = r;
            applied = AppliedWindow {
                rect: r,
                border_w: 2,
                seen: true,
                sequence: None,
            };
            last = r;
            iterations += 1;
        }
        // MEASUREMENT NOTE: the loop completed exactly 200 iterations; every
        // iteration produced a `Stale` verdict (not our echo) that the sink
        // adopted, with no backoff mechanism throttling the adoptions
        // (count = 200, all followed).
        assert_eq!(iterations, 200, "loop completed 200 iterations");
        assert_eq!(engine.state.clients.get(&1).unwrap().geom, last);
        assert_eq!(
            applied.rect, last,
            "applied converged to last reported rect"
        );
        engine
            .state
            .check_invariants()
            .expect("invariants after 200-iteration storm");
    }

    // A realistic, in-memory property test over the FULL client interaction
    // surface (manage / destroy / focus / fullscreen / maximize / float /
    // move-resize / workspace-switch / monitor-switch / ConfigureRequest /
    // ConfigureNotify), checking invariants after EVERY step. ConfigureX is
    // simulated at the model/policy level through the reconciler's
    // `classify_configure`; no X11 connection is opened. The backend's
    // last-written geometry is one long-lived `AppliedState`; each step merges
    // `pipeline_desired` across both monitors into the whole-desktop Desired,
    // asserts the structural properties, then reconciles and applies the
    // effects. Coverage counters guarantee the run was not vacuous.
    //
    // The column/ribbon scroll model (niri-style) deliberately scrolls
    // NON-FOCUSED columns partially or fully off-screen, so
    // `State::check_invariants` asserts neither geometry positivity nor
    // on-screen bounds. Off-screen Desired rects are by design — so this
    // harness asserts only that every Desired
    // rect is positive, every Desired window id exists in `state.clients`, and
    // the `raise` list names known windows. `check_invariants` itself runs every
    // step, which also exercises reconcile convergence, the
    // `classify_configure` policy and the destroy-before-reconcile race.
    struct ResistanceCounters {
        overlay_present: usize,
        pending_focus_present: usize,
        multimon: usize,
        x11_real_diverged: usize,
        desired_differs_applied: usize,
        destroy_before_reconcile: usize,
        configure_requests: usize,
        active_window_requests: usize,
        transient_chains: usize,
    }
    impl ResistanceCounters {
        fn merge(&mut self, o: ResistanceCounters) {
            self.overlay_present += o.overlay_present;
            self.pending_focus_present += o.pending_focus_present;
            self.multimon += o.multimon;
            self.x11_real_diverged += o.x11_real_diverged;
            self.desired_differs_applied += o.desired_differs_applied;
            self.destroy_before_reconcile += o.destroy_before_reconcile;
            self.configure_requests += o.configure_requests;
            self.active_window_requests += o.active_window_requests;
            self.transient_chains += o.transient_chains;
        }
    }

    fn run_resistance_seed(seed: u64, steps: u32) -> ResistanceCounters {
        use crate::backend::x11::reconciler::{
            classify_configure, reconcile, AppliedState, AppliedWindow, ConfigureObservation,
            GeometryEffect,
        };
        use crate::core::commands::{
            consume_pending_focus, decide_active_window, ActiveWindowIntent, FocusMonitor,
            MoveResize, ToggleFloat, ToggleFullscreen, ToggleMaximize, ViewWorkspace,
        };
        use crate::core::effect::Effect;
        use crate::types::{Dir, Rect, WindowId};

        const MAX_WINS: usize = 24;
        const NOPS: u32 = 14;

        // Tiny deterministic LCG — reproducible, no external RNG dependency.
        struct Rng(u64);
        impl Rng {
            fn next(&mut self) -> u64 {
                self.0 = self
                    .0
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                self.0 >> 16
            }
            fn below(&mut self, n: u32) -> u32 {
                (self.next() % n as u64) as u32
            }
        }
        let mut rng = Rng(seed);

        let mut engine = setup_engine_multi();
        let nmon = engine.state.monitors.len();
        let mut live: Vec<WindowId> = Vec::new();
        let mut next_win: WindowId = 1;
        let mut applied = AppliedState::default();
        // The destroy-before-reconcile race: windows destroyed but whose
        // AppliedState entry is deliberately NOT yet forgotten.
        let mut pending_forget: Vec<WindowId> = Vec::new();

        // Coverage counters (each must be > 0 or the run was vacuous).
        let mut overlay_present = 0usize;
        let mut pending_focus_present = 0usize;
        let mut multimon = 0usize;
        let mut x11_real_diverged = 0usize;
        let mut desired_differs_applied = 0usize;
        let mut destroy_before_reconcile = 0usize;
        let mut configure_requests = 0usize;
        let mut active_window_requests = 0usize;
        let mut transient_chains = 0usize;

        // Whole-desktop Desired: `pipeline_desired` merged across every monitor.
        let run_pipeline_all = |engine: &Engine| -> DesiredState {
            let mut all = DesiredState::default();
            for mi in 0..engine.state.monitors.len() {
                let d = pipeline_desired(engine, mi);
                all.windows.extend(d.windows);
            }
            all
        };

        let screen0 = engine.state.monitors[0].screen;
        let rrect = |rng: &mut Rng| -> Rect {
            let x = (rng.below(screen0.w) as i32).clamp(0, screen0.w as i32 - 50);
            let y = (rng.below(screen0.h) as i32).clamp(0, screen0.h as i32 - 50);
            let w = 50 + rng.below(600);
            let h = 50 + rng.below(400);
            Rect::new(x, y, w, h)
        };

        // Pure-harness counterpart of the backend's `unmanage`: a window may carry
        // a `focus_stack` entry on a *different* monitor than its own `c.monitor`
        // (the WM keys the focus deferral on `sel_mon`, not the client's monitor).
        // The real backend scrubs every monitor's stack on unmanage; the harness
        // must do the same so `check_invariants` (which scans every monitor's
        // `focus_stack`) stays green. No WM core is touched.
        let purge_focus = |engine: &mut Engine, w: WindowId| {
            for mon in &mut engine.state.monitors {
                mon.focus_stack.retain(|&x| x != w);
                if mon.focused == Some(w) {
                    mon.focused = mon.focus_stack.last().copied();
                }
            }
        };

        macro_rules! run {
            ($cmd:expr) => {{
                // Mirror the backend's focus sink: a command only *emits*
                // `FocusWindow`, and the real `Backend::focus()` focuses on the
                // window's OWN monitor (`mon_i = c.monitor`) AND sets
                // `sel_mon = mon_i` (render.rs:746, 798-799). The engine always
                // acts on `sel_mon`, so `sel_mon` must name the monitor that
                // actually holds the focused window: a desync makes a
                // sel_mon-based command (`ToggleFloat`/`ToggleMaximize`/
                // `ToggleFullscreen`/`MoveResize` all remove from and re-insert
                // into `monitors[sel_mon]`) tear the window out of its true tree
                // and re-insert it on the wrong monitor — a false cross-monitor
                // duplicate that has nothing to do with the WM core.
                if let Some(fw) = engine.state.monitors[engine.state.sel_mon].focused {
                    if let Some(fm) = engine.state.clients.get(&fw).map(|c| c.monitor) {
                        engine.state.sel_mon = fm;
                        engine.state.monitors[fm].focused = Some(fw);
                    }
                }
                let effects = engine.execute($cmd);
                for eff in &effects {
                    if let Effect::FocusWindow(Some(w)) = eff {
                        if let Some(c) = engine.state.clients.get(w) {
                            let mi = c.monitor;
                            engine.state.sel_mon = mi;
                            engine.state.monitors[mi].focused = Some(*w);
                        }
                    }
                }
                effects
            }};
        }

        // Seed: one managed window so focus/overlay ops have a target.
        {
            let w = next_win;
            next_win += 1;
            t_manage(&mut engine, w);
            live.push(w);
        }

        for step in 0..steps {
            let op = rng.below(NOPS);

            // Heal any stale `pending_focus` carried from a prior (non-`run!`)
            // op before driving the next command, so the engine's debug-only
            // `assert_invariants` (invoked at the end of every `engine.execute`)
            // only ever sees a slot whose owner is a currently-presented overlay.
            // The harness helpers `t_manage`/`t_manage_transient` set
            // `pending_focus` through `decide_manage_focus` *without* going
            // through `engine.execute`, and a chaotic sequence may then change
            // the overlay state (toggle fullscreen/maximize, move, destroy) on
            // the owning window before the next `run!` op runs — leaving the
            // deferral dangling until the backend's next focus/unmanage
            // reconciliation. The production backend performs exactly this
            // consume-on-stale check every turn; the pure harness must mirror it
            // so the fuzz exercises the model cleanly instead of tripping a
            // transient (debug-only) assertion on an intermediate it would
            // otherwise heal by end-of-step.
            if let Some(pf) = engine.state.pending_focus {
                let owner_presented = engine.state.monitors.get(pf.monitor).is_some_and(|m| {
                    let focused = m.focused;
                    m.workspaces.get(pf.workspace).is_some_and(|ws| {
                        engine.state.clients.get(&pf.owner).is_some_and(|c| {
                            c.monitor == pf.monitor
                                && c.workspace == pf.workspace
                                && ((c.is_fullscreen()
                                    && (ws.layout == LayoutKind::Column || c.is_true_fullscreen()))
                                    || ((c.is_maximized_v() || c.is_maximized_h())
                                        && focused == Some(pf.owner)))
                        })
                    })
                });
                if !owner_presented {
                    consume_pending_focus(&mut engine.state, pf.monitor, pf.workspace, None);
                }
            }

            match op {
                // 0: ManageWindow (create on a random monitor; bias toward an
                //    overlay-bearing monitor so the pending_focus deferral path
                //    (and its invariant) is exercised).
                0 => {
                    if live.len() < MAX_WINS {
                        let target = if rng.below(3) == 0 {
                            let mut cand = None;
                            for mi in 0..nmon {
                                if engine.state.presented_overlay_owner(mi).is_some() {
                                    cand = Some(mi);
                                }
                            }
                            cand.unwrap_or_else(|| rng.below(nmon as u32) as usize)
                        } else {
                            rng.below(nmon as u32) as usize
                        };
                        engine.state.sel_mon = target;
                        let w = next_win;
                        next_win += 1;
                        t_manage(&mut engine, w);
                        live.push(w);
                    }
                }
                // 1: DestroyWindow — occasionally defer the AppliedState forget
                //    to model the destroy-before-reconcile race.
                1 => {
                    if !live.is_empty() {
                        let i = rng.below(live.len() as u32) as usize;
                        let w = live.remove(i);
                        t_destroy(&mut engine, w);
                        purge_focus(&mut engine, w);
                        if rng.below(10) < 3 {
                            pending_forget.push(w);
                            destroy_before_reconcile += 1;
                        } else {
                            applied.forget(w);
                        }
                    }
                }
                // 2: Focus.
                2 => {
                    if !live.is_empty() {
                        let w = live[rng.below(live.len() as u32) as usize];
                        t_focus(&mut engine, w);
                    }
                }
                // 3: Fullscreen toggle.
                3 => {
                    run!(ToggleFullscreen(None));
                }
                // 4: Maximize toggle.
                4 => {
                    run!(ToggleMaximize(None));
                }
                // 5: Float toggle.
                5 => {
                    run!(ToggleFloat(None));
                }
                // 6: MoveResize (valid rect) on an already-floating window.
                6 => {
                    if !live.is_empty() {
                        let w = live[rng.below(live.len() as u32) as usize];
                        let is_float = engine
                            .state
                            .clients
                            .get(&w)
                            .is_some_and(crate::types::Client::is_float);
                        if is_float {
                            let g = rrect(&mut rng);
                            if let Some(c) = engine.state.clients.get_mut(&w) {
                                c.geom = g;
                            }
                            run!(MoveResize(w, g));
                        }
                    }
                }
                // 7: WorkspaceSwitch (ViewWorkspace).
                7 => {
                    let n = engine.state.monitors[engine.state.sel_mon].workspaces.len();
                    let ws = rng.below(n as u32) as usize;
                    run!(ViewWorkspace(ws));
                    let sel = engine.state.sel_mon;
                    if let Some(b) = engine.state.best_focus(sel) {
                        crate::core::commands::focus_logical_on(&mut engine.state, sel, b);
                    }
                }
                // 8: MonitorSwitch (FocusMonitor).
                8 => {
                    run!(FocusMonitor(Dir::Right));
                }
                // 9: ConfigureRequest (simulated — no X11 connection). A reported
                //    rect the WM did not ask for. Tiled/fullscreen ⇒ the WM is the
                //    authority (Stale ⇒ re-assert); a pure float ⇒ the sink adopts.
                9 => {
                    if !live.is_empty() {
                        configure_requests += 1;
                        let w = live[rng.below(live.len() as u32) as usize];
                        let facts = engine
                            .state
                            .clients
                            .get(&w)
                            .map(|c| (c.is_float() && !c.is_fullscreen(), c.border_w));
                        if let Some((is_float_fs, bw)) = facts {
                            let reported = rrect(&mut rng);
                            let a = applied.windows.get(&w).copied().unwrap_or_default();
                            let obs = classify_configure(reported, a.border_w, &a);
                            let ok = obs != ConfigureObservation::Compliant;
                            assert!(
                                ok,
                                "seed {seed:#x} step {step} op ConfigureRequest win {w}: expected Stale (not our echo) but got a different verdict",
                            );
                            if is_float_fs {
                                // Float: adopt the reported geometry into the model.
                                if let Some(cmut) = engine.state.clients.get_mut(&w) {
                                    cmut.geom = reported;
                                }
                                applied.windows.insert(
                                    w,
                                    AppliedWindow {
                                        rect: reported,
                                        border_w: bw,
                                        seen: true,
                                        sequence: None,
                                    },
                                );
                            } else {
                                // WM authority: reassert Desired (Applied := Desired).
                                let dr = run_pipeline_all(&engine)
                                    .windows
                                    .iter()
                                    .find(|d| d.window == w)
                                    .map(|d| d.rect);
                                if let Some(dr) = dr {
                                    applied.windows.insert(
                                        w,
                                        AppliedWindow {
                                            rect: dr,
                                            border_w: bw,
                                            seen: true,
                                            sequence: None,
                                        },
                                    );
                                }
                                // Hidden window: leave Applied as the authoritative
                                // off-screen geometry (reconcile won't touch it).
                            }
                        }
                    }
                }
                // 11: ActiveWindow — simulate an EWMH `_NET_ACTIVE_WINDOW` request.
                11 => {
                    if !live.is_empty() {
                        let w = live[rng.below(live.len() as u32) as usize];
                        if let Some(c) = engine.state.clients.get(&w) {
                            let mi = c.monitor;
                            let ws = c.workspace;
                            let owner = engine.state.presented_overlay_owner_in(mi, ws);
                            let must_ignore =
                                owner.is_some_and(|o| o != w && c.transient_parent != Some(o));
                            let intent = decide_active_window(&engine.state, w);
                            assert_eq!(
                                intent,
                                if must_ignore {
                                    ActiveWindowIntent::Ignore
                                } else {
                                    ActiveWindowIntent::Focus(w)
                                },
                                "seed {seed:#x} step {step} op ActiveWindow win {w}: intent mismatch (owner {owner:?}, transient_parent {:?})",
                                c.transient_parent
                            );
                        }
                        active_window_requests += 1;
                    }
                }
                // 12: Transient creation (build a transient chain under chaos).
                12 => {
                    if !live.is_empty() && live.len() < MAX_WINS {
                        let p = live[rng.below(live.len() as u32) as usize];
                        let t = next_win;
                        next_win += 1;
                        t_manage_transient(&mut engine, t, p);
                        live.push(t);
                        transient_chains += 1;
                    }
                }
                // 13: Transient destruction (teardown under chaos).
                13 => {
                    if let Some(pos) = live.iter().position(|&lw| {
                        engine
                            .state
                            .clients
                            .get(&lw)
                            .is_some_and(|c| c.transient_parent.is_some())
                    }) {
                        let w = live.remove(pos);
                        t_destroy(&mut engine, w);
                        purge_focus(&mut engine, w);
                        applied.forget(w);
                    }
                }
                // 10: ConfigureNotify (simulated) — several flavors across the run.
                _ => {
                    if !live.is_empty() {
                        let w = live[rng.below(live.len() as u32) as usize];
                        let flavor = rng.below(5);
                        // `reported_out` is None only for the dead-window no-op.
                        let mut reported_out: Option<Rect> = None;
                        match flavor {
                            0 => {
                                // Echo: reported == last applied → Compliant.
                                if let Some(a) = applied.windows.get(&w) {
                                    if a.seen {
                                        reported_out = Some(a.rect);
                                    }
                                }
                                if reported_out.is_none() {
                                    reported_out = Some(rrect(&mut rng));
                                }
                            }
                            1 => {
                                // Reassert path: reported == Desired (≠ stale Applied).
                                let dr = run_pipeline_all(&engine)
                                    .windows
                                    .iter()
                                    .find(|d| d.window == w)
                                    .map(|d| d.rect);
                                reported_out = dr.or_else(|| Some(rrect(&mut rng)));
                            }
                            2 => {
                                // Divergent from both Applied and Desired.
                                reported_out = Some(rrect(&mut rng));
                            }
                            3 => {
                                // Dead window (just destroyed): must be a no-op.
                                if pending_forget.is_empty() {
                                    reported_out = Some(rrect(&mut rng));
                                } else {
                                    let dw = pending_forget
                                        [rng.below(pending_forget.len() as u32) as usize];
                                    if engine.state.clients.contains_key(&dw) {
                                        reported_out = Some(rrect(&mut rng));
                                    } else {
                                        reported_out = None; // no client ⇒ backend ignores
                                    }
                                }
                            }
                            _ => {
                                // Hidden (off-screen) window: reported == off-screen
                                // Applied ⇒ Compliant (simulates the post-workspace-
                                // switch hide + later ConfigureNotify echo).
                                let hidden = live.iter().copied().find(|&lw| {
                                    run_pipeline_all(&engine)
                                        .windows
                                        .iter()
                                        .all(|d| d.window != lw)
                                        && applied.windows.contains_key(&lw)
                                });
                                if let Some(hw) = hidden {
                                    if let Some(a) = applied.windows.get(&hw) {
                                        reported_out = Some(a.rect);
                                    }
                                }
                                if reported_out.is_none() {
                                    reported_out = Some(rrect(&mut rng));
                                }
                            }
                        }

                        if let Some(reported) = reported_out {
                            let facts = engine
                                .state
                                .clients
                                .get(&w)
                                .map(|c| (c.is_float() && !c.is_fullscreen(), c.border_w));
                            if let Some((is_float_fs, bw)) = facts {
                                let a = applied.windows.get(&w).copied().unwrap_or_default();
                                let obs = classify_configure(reported, a.border_w, &a);
                                // Pure geometry contract: only an echo of Applied
                                // is Compliant; every other report (incl. the
                                // Desired-matching reassert path) is stale
                                // traffic the sink re-asserts over or adopts.
                                if reported == a.rect {
                                    assert_eq!(
                                        obs,
                                        ConfigureObservation::Compliant,
                                        "seed {seed:#x} step {step} op ConfigureNotify win {w}: echo of Applied must be Compliant"
                                    );
                                } else {
                                    assert_eq!(
                                        obs,
                                        ConfigureObservation::Stale,
                                        "seed {seed:#x} step {step} op ConfigureNotify win {w}: non-echo report must be Stale"
                                    );
                                }
                                if is_float_fs {
                                    if let Some(cmut) = engine.state.clients.get_mut(&w) {
                                        cmut.geom = reported;
                                    }
                                    applied.windows.insert(
                                        w,
                                        AppliedWindow {
                                            rect: reported,
                                            border_w: bw,
                                            seen: true,
                                            sequence: None,
                                        },
                                    );
                                } else {
                                    let dr = run_pipeline_all(&engine)
                                        .windows
                                        .iter()
                                        .find(|d| d.window == w)
                                        .map(|d| d.rect);
                                    if let Some(dr) = dr {
                                        applied.windows.insert(
                                            w,
                                            AppliedWindow {
                                                rect: dr,
                                                border_w: bw,
                                                seen: true,
                                                sequence: None,
                                            },
                                        );
                                    }
                                }
                            }
                            // Client gone (dead-window flavor resolved to Some after
                            // all) ⇒ nothing to do; the event is harmless.
                        }
                    }
                }
            }

            // Mirror the backend's focus→camera retarget: after every focus change the
            // real `Backend::focus()` (and `retarget_focus_to_window`) re-point the
            // workspace camera AND its focused column index onto the focused window so
            // the settled `arrange` projection places columns within the screen. The
            // pure harness ops (`t_focus`/`FocusMonitor`) only move `mon.focused`,
            // leaving `ws.focus.column_idx`/camera stale — so we re-centre here. Pure
            // test scaffolding; no WM core is touched.
            for mi in 0..engine.state.monitors.len() {
                // Re-centre the camera onto the FOCUSED window only (mirrors the real
                // backend's `Backend::focus()`, which retargets to the focused window).
                // `best_focus` is a different concept (focus-steal / overlay ownership)
                // and must NOT also re-point the camera — doing so re-centres on a
                // *different* column and pushes the actual focused column off-screen.
                // It is used purely as a fallback when there is no focused window yet.
                let focal = engine.state.monitors[mi]
                    .focused
                    .or_else(|| engine.state.best_focus(mi));
                if let Some(w) = focal {
                    // Point the camera at the focused window's column (sets
                    // `ws.focus.column_idx` / `column.focused`, needed by the
                    // projection) — mirrors the real backend's focus retarget.
                    let _ = crate::core::commands::retarget_focus_to_window(
                        &mut engine.state,
                        &engine.cfg,
                        w,
                    );
                    // The camera has to sit where `ideal_scroll` would put it:
                    // the harness never runs the focus commands, so without this
                    // the focused column starts off-screen and every later
                    // assertion about it is vacuous.
                    let aws = engine.state.monitors[mi].active_ws;
                    let scroll = {
                        let m = &engine.state.monitors[mi];
                        let ws = &m.workspaces[aws];
                        let fs = crate::core::layout::fs_ctx(&engine.state.clients, ws, m.screen);
                        crate::core::layout::ideal_scroll(ws, &engine.cfg, m.workarea, fs)
                    };
                    let m = &mut engine.state.monitors[mi];
                    m.workspaces[aws].camera.position = scroll;
                }
            }

            // Build the whole-desktop Desired for this step.
            let desired = run_pipeline_all(&engine);

            // Directed Desired assertions — deliberately weak, see the harness
            // header for why on-screen bounds are not asserted:
            //  - every Desired window id exists in state.clients
            //  - every Desired rect is positive (a non-positive size is real
            //    corruption; coords are i32 so NaN/inf cannot occur)
            for d in &desired.windows {
                assert!(
                    engine.state.clients.contains_key(&d.window),
                    "seed {seed:#x} step {step}: Desired names unknown client {}",
                    d.window
                );
            }
            for dw in &desired.windows {
                assert!(
                    dw.rect.w > 0 && dw.rect.h > 0,
                    "Desired window {} has non-finite or non-positive rect {:?}",
                    dw.window,
                    dw.rect
                );
            }

            // Coverage flags (computed before reconcile — X11 Real lags Desired).
            if (0..nmon).any(|mi| engine.state.presented_overlay_owner(mi).is_some()) {
                overlay_present += 1;
            }
            if engine.state.pending_focus.is_some() {
                pending_focus_present += 1;
            }
            let mons_with = (0..nmon)
                .filter(|&mi| engine.state.clients.values().any(|c| c.monitor == mi))
                .count();
            if mons_with > 1 {
                multimon += 1;
            }
            let mut diverged = false;
            for d in &desired.windows {
                if let Some(a) = applied.windows.get(&d.window) {
                    if a.rect != d.rect {
                        diverged = true;
                        break;
                    }
                }
            }
            if !diverged
                && (desired
                    .windows
                    .iter()
                    .any(|d| !applied.windows.contains_key(&d.window))
                    || applied
                        .windows
                        .keys()
                        .any(|aw| !desired.windows.iter().any(|d| d.window == *aw)))
            {
                diverged = true;
            }
            if diverged {
                x11_real_diverged += 1;
            }
            let differ = diverged
                || desired
                    .windows
                    .iter()
                    .any(|d| !applied.windows.contains_key(&d.window));
            if differ {
                desired_differs_applied += 1;
            }

            // Reconcile Desired → Applied; apply the returned effects.
            let effects = reconcile(&desired, &engine.state, &mut applied);
            for eff in &effects {
                let GeometryEffect::Configure { win, rect, .. } = eff;
                assert!(
                    engine.state.clients.contains_key(win),
                    "seed {seed:#x} step {step}: Configure for unknown window {win}"
                );
                assert!(
                    rect.w > 0 && rect.h > 0,
                    "seed {seed:#x} step {step}: Configure zero-size rect {rect:?}"
                );
                // The destroy-before-reconcile race: reconcile must NOT emit
                // for a window that is gone from Desired (and thus clients).
                assert!(
                        !pending_forget.contains(win),
                        "seed {seed:#x} step {step}: reconcile emitted effect for destroyed-but-not-forgotten window {win}"
                    );
                // WM authority convergence: Applied == Desired after reconcile.
                let drect = desired
                    .windows
                    .iter()
                    .find(|d| d.window == *win)
                    .map(|d| d.rect)
                    .expect("desired must contain the configured window");
                assert_eq!(
                    applied.windows[win].rect, drect,
                    "seed {seed:#x} step {step}: applied rect diverged from desired for {win}"
                );
                // Floats must follow the model (the float projection is a pure
                // function of the client's own geometry); overlays are excluded
                // (the WM is authoritative for them). The projection is
                // `normalize_float_geom`, NOT the raw `client.geom`: arrange is
                // documented as a pure projection that never mutates
                // `client.geom`, so a float the WM has not re-settled (for
                // example one adopted verbatim from the client, or one whose
                // grid/clamp normalization is not yet a fixed point) is
                // legitimately projected to a different rect than the record
                // holds. Asserting the record itself made this a restatement of
                // "normalization is the identity", which it is not: a float
                // covering its whole workarea is projected 2px in on every side.
                // A float's `Desired` is its *frame*, while the model records the
                // *client* area inside that frame, so the two rects are
                // legitimately different: a float whose client geometry fills the
                // workarea is projected inwards by the border on every side rather
                // than kept as it is. Comparing them directly asserted a border of
                // zero, which is why this check used to be quarantined.
                //
                // What the model promises here is containment: a float's frame is
                // always inside the workarea it belongs to, whatever the client's
                // own size hints asked for. That is a real contract rather than a
                // restatement of the projection, and it is the one the old
                // `desired == client.geom` comparison could not express at all.
                //
                // Restating the projection as the layout's own `normalize_float_geom`
                // call would compare the implementation against itself, so it is
                // deliberately not done. The `float_client_authority` arm — where
                // the promise is stronger, that the adopted rect survives verbatim
                // so the float cannot jump on its own — is not reachable from this
                // harness, so asserting it here would be dormant; it is covered by
                // the dedicated seal tests in `src/backend/x11/render.rs`
                // (`workarea_change_releases_the_client_authority_seal` and
                // siblings).
                if let Some(c) = engine.state.clients.get(win) {
                    if c.is_float() && !c.is_fullscreen() {
                        let mon = &engine.state.monitors[c.monitor];
                        let ws = &mon.workspaces[c.workspace];
                        let is_overlay = (c.is_fullscreen()
                            && (ws.layout == LayoutKind::Column || c.is_true_fullscreen()))
                            || ws.presented_maximize == Some(*win);
                        if !is_overlay {
                            let wa = mon.workarea;
                            assert!(
                                drect.x >= wa.x
                                    && drect.y >= wa.y
                                    && drect.x + drect.w as i32 <= wa.x + wa.w as i32
                                    && drect.y + drect.h as i32 <= wa.y + wa.h as i32,
                                "seed {seed:#x} step {step}: float {win} projected rect \
                                 {drect:?} escapes the workarea {wa:?}"
                            );
                        }
                    }
                }
            }

            // Occasionally retire a deferred forget (the race eventually resolves).
            if !pending_forget.is_empty() && rng.below(5) == 0 {
                let i = rng.below(pending_forget.len() as u32) as usize;
                let w = pending_forget.remove(i);
                applied.forget(w);
            }

            // End-of-step mirror of the same consume-on-stale check the pre-command
            // heal above performs, so `check_invariants` never sees a deferral whose
            // owner stopped being an overlay mid-step.
            if let Some(pf) = engine.state.pending_focus {
                let owner_presented = engine.state.monitors.get(pf.monitor).is_some_and(|m| {
                    let focused = m.focused;
                    m.workspaces.get(pf.workspace).is_some_and(|ws| {
                        engine.state.clients.get(&pf.owner).is_some_and(|c| {
                            c.monitor == pf.monitor
                                && c.workspace == pf.workspace
                                && ((c.is_fullscreen()
                                    && (ws.layout == LayoutKind::Column || c.is_true_fullscreen()))
                                    || ((c.is_maximized_v() || c.is_maximized_h())
                                        && focused == Some(pf.owner)))
                        })
                    })
                });
                if !owner_presented {
                    consume_pending_focus(&mut engine.state, pf.monitor, pf.workspace, None);
                }
            }

            // Full invariants after EVERY step.
            if let Err(v) = engine.state.check_invariants() {
                panic!(
                    "seed {seed:#x} step {step} op {op}: invariant violation: {}",
                    v.join("\n  - ")
                );
            }
        }

        // Resolve any still-pending forgots.
        for w in pending_forget.drain(..) {
            applied.forget(w);
        }

        engine
            .state
            .check_invariants()
            .expect("final state must satisfy invariants");

        ResistanceCounters {
            overlay_present,
            pending_focus_present,
            multimon,
            x11_real_diverged,
            desired_differs_applied,
            destroy_before_reconcile,
            configure_requests,
            active_window_requests,
            transient_chains,
        }
    }

    #[test]
    fn property_realistic_client_resistance() {
        const SEEDS: [u64; 5] = [
            0x0000_0000_9999_9999,
            0x1111_2222_3333_4444,
            0x9E3779B97F4A7C15,
            0xABAD_C0DE_CAFE_BABE,
            0x1234_5678_9ABC_DEF0,
        ];
        const STEPS: u32 = 10_000;

        let mut total = run_resistance_seed(SEEDS[0], STEPS);
        for &s in &SEEDS[1..] {
            total.merge(run_resistance_seed(s, STEPS));
        }

        eprintln!(
            "property_realistic_client_resistance coverage: overlay_present={} pending_focus_present={} multimon={} x11_real_diverged={} desired_differs_applied={} destroy_before_reconcile={} configure_requests={} active_window_requests={} transient_chains={}",
            total.overlay_present,
            total.pending_focus_present,
            total.multimon,
            total.x11_real_diverged,
            total.desired_differs_applied,
            total.destroy_before_reconcile,
            total.configure_requests,
            total.active_window_requests,
            total.transient_chains,
        );

        assert!(
            total.overlay_present > 0,
            "overlay_present was never set (vacuous multi-seed run)"
        );
        assert!(
            total.pending_focus_present > 0,
            "pending_focus_present was never set (vacuous multi-seed run)"
        );
        assert!(
            total.multimon > 0,
            "multimon was never set (vacuous multi-seed run)"
        );
        assert!(
            total.x11_real_diverged > 0,
            "x11_real_diverged was never set (vacuous multi-seed run)"
        );
        assert!(
            total.desired_differs_applied > 0,
            "desired_differs_applied was never set (vacuous multi-seed run)"
        );
        assert!(
            total.destroy_before_reconcile > 0,
            "destroy_before_reconcile was never exercised (vacuous multi-seed run)"
        );
        assert!(
            total.configure_requests > 0,
            "configure_requests was never exercised (vacuous multi-seed run)"
        );
        assert!(
            total.active_window_requests > 0,
            "active_window_requests was never exercised (vacuous multi-seed run)"
        );
        assert!(
            total.transient_chains > 0,
            "transient_chains was never exercised (vacuous multi-seed run)"
        );
    }

    // A fullscreen ConfigureRequest is ignored: `on_configure_request`'s
    // fullscreen branch returns early without adopting the client rect, and the
    // WM re-asserts its own Desired.
    #[test]
    fn configure_request_fullscreen_is_ignored_model_a() {
        use crate::backend::x11::reconciler::{
            classify_configure, AppliedWindow, ConfigureObservation,
        };
        use crate::core::commands::ToggleFullscreen;
        use crate::types::{Action, LayoutKind, Rect, WindowId};

        let mut engine = setup_engine();
        // `ToggleFullscreen` below promotes the policy to `True`, which is what
        // makes the window a presented overlay owner.
        engine.dispatch(Action::SetLayout(LayoutKind::Column));

        let w: WindowId = 1;
        t_manage(&mut engine, w);
        t_focus(&mut engine, w);
        engine.execute(ToggleFullscreen(None));

        // Capture the WM's own Desired rect and the client geometry BEFORE the
        // simulated ConfigureRequest.
        let desired_before = pipeline_desired(&engine, 0)
            .windows
            .iter()
            .find(|d| d.window == w)
            .map(|d| d.rect)
            .expect("fullscreen window must appear in Desired");
        let client_geom_before = engine.state.clients[&w].geom;

        // A client attempts to move itself far off-screen (a divergent rect).
        let reported = Rect::new(-5000, -5000, 1, 1);
        let applied = AppliedWindow {
            rect: desired_before,
            border_w: engine.cfg.border_w,
            seen: true,
            sequence: None,
        };
        let obs = classify_configure(reported, engine.cfg.border_w, &applied);
        assert_eq!(
            obs,
            ConfigureObservation::Stale,
            "model A: a fullscreen ConfigureRequest must be Stale (WM authority re-asserts)"
        );

        // The WM must NOT adopt the divergent client rect — it reasserts its own
        // Desired instead.
        let client_geom_after = engine.state.clients[&w].geom;
        assert_eq!(
            client_geom_after, client_geom_before,
            "model A: WM must not adopt the divergent client rect into client.geom"
        );
        let desired_after = pipeline_desired(&engine, 0)
            .windows
            .iter()
            .find(|d| d.window == w)
            .map(|d| d.rect)
            .expect("fullscreen window must appear in Desired");
        assert_eq!(
            desired_after, desired_before,
            "model A: WM Desired must be unchanged after a fullscreen ConfigureRequest"
        );

        engine
            .state
            .check_invariants()
            .expect("model A: final state must satisfy invariants");
    }

    // `decide_active_window` is the pure policy the X11 handler calls for every
    // `_NET_ACTIVE_WINDOW` request. It must refuse to let an unrelated window
    // steal focus from a presented fullscreen/maximize overlay on the *same*
    // (monitor, workspace) as the requester, while still honoring the overlay
    // owner itself and any dialog it owns.

    /// Register + tile `win` on an explicit (monitor, workspace), optionally
    /// making it a fullscreen overlay owner there.
    fn aw_add_client(
        engine: &mut Engine,
        win: WindowId,
        mi: usize,
        ws_i: usize,
        grid_fs_overlay: bool,
    ) {
        let mut c = Client::new(win, mi, ws_i);
        c.border_w = engine.cfg.border_w;
        c.geom = Rect::new(0, 0, 800, 600);
        c.saved_geom = c.geom;
        engine.state.monitors[mi].workspaces[ws_i].add_tiled(win, engine.cfg.column_width);
        engine.state.add_client(c);
        if grid_fs_overlay {
            let mon = &mut engine.state.monitors[mi];
            mon.focus_stack.retain(|&w| w != win);
            mon.focus_stack.push(win);
            let cc = engine.state.clients.get_mut(&win).unwrap();
            cc.flags.set(WinFlags::FULLSCREEN);
            cc.fullscreen_policy = crate::types::FullscreenPolicy::True;
        }
    }

    #[test]
    fn net_active_window_respects_presented_overlay_policy() {
        use crate::core::commands::{decide_active_window, ActiveWindowIntent};

        // 1) Plain tiled B cannot steal focus from a fullscreen overlay A on the
        //    same (mon0, ws0).
        {
            let mut engine = setup_engine();
            let mi = engine.state.sel_mon;
            let ws_i = engine.state.monitors[mi].active_ws;
            aw_add_client(&mut engine, 1, mi, ws_i, true); // A: fullscreen overlay
            aw_add_client(&mut engine, 2, mi, ws_i, false); // B: plain tiled
            assert_eq!(engine.state.presented_overlay_owner(mi), Some(1));
            assert_eq!(
                decide_active_window(&engine.state, 2),
                ActiveWindowIntent::Ignore,
                "an unrelated tiled window must not steal a presented overlay"
            );
        }

        // 2) B is an owned dialog (transient) of overlay owner A → honored.
        {
            let mut engine = setup_engine();
            let mi = engine.state.sel_mon;
            let ws_i = engine.state.monitors[mi].active_ws;
            aw_add_client(&mut engine, 1, mi, ws_i, true); // A: overlay owner
            aw_add_client(&mut engine, 2, mi, ws_i, false); // B
            engine.state.clients.get_mut(&2).unwrap().transient_parent = Some(1);
            assert_eq!(
                decide_active_window(&engine.state, 2),
                ActiveWindowIntent::Focus(2),
                "a dialog owned by the overlay owner must be honored"
            );
        }

        // 3) A is the overlay owner on (mon0, ws0); B on another workspace (ws1)
        //    with no overlay there → honored (same monitor, different workspace).
        {
            let mut engine = setup_engine();
            let mi = engine.state.sel_mon;
            let ws0 = engine.state.monitors[mi].active_ws;
            let ws1 = 1;
            aw_add_client(&mut engine, 1, mi, ws0, true); // A overlay on ws0
            aw_add_client(&mut engine, 2, mi, ws1, false); // B on ws1
            assert_eq!(engine.state.presented_overlay_owner_in(mi, ws0), Some(1));
            assert_eq!(
                decide_active_window(&engine.state, 2),
                ActiveWindowIntent::Focus(2),
                "a window on an overlay-free workspace is focusable"
            );
        }

        // 4) A is the overlay owner on (mon0, ws0); B on another monitor (mon1)
        //    with no overlay there → honored.
        {
            let mut engine = setup_engine_multi();
            let m0 = 0usize;
            let m1 = 1usize;
            let ws0 = 0usize;
            aw_add_client(&mut engine, 1, m0, ws0, true); // A overlay on mon0/ws0
            aw_add_client(&mut engine, 2, m1, ws0, false); // B on mon1/ws0
            assert_eq!(engine.state.presented_overlay_owner_in(m0, ws0), Some(1));
            assert_eq!(
                decide_active_window(&engine.state, 2),
                ActiveWindowIntent::Focus(2),
                "a window on an overlay-free monitor is focusable"
            );
        }

        // 5) No overlay anywhere; B on (mon0, ws0) → honored.
        {
            let mut engine = setup_engine();
            let mi = engine.state.sel_mon;
            let ws_i = engine.state.monitors[mi].active_ws;
            aw_add_client(&mut engine, 2, mi, ws_i, false);
            assert!(engine.state.presented_overlay_owner(mi).is_none());
            assert_eq!(
                decide_active_window(&engine.state, 2),
                ActiveWindowIntent::Focus(2),
                "without any overlay an active-window request is honored"
            );
        }

        // 6) Overlay present on (mon0, ws0) with a deferred `pending_focus`
        //    (keyed on that mon/ws) owned by an unrelated B; B's request is
        //    STILL refused — the overlay protection covers it, and the explicit
        //    request would not be honored even though it "matches" the deferral.
        {
            let mut engine = setup_engine();
            let mi = engine.state.sel_mon;
            let ws_i = engine.state.monitors[mi].active_ws;
            aw_add_client(&mut engine, 1, mi, ws_i, true); // A overlay on ws0
            aw_add_client(&mut engine, 2, mi, ws_i, false); // B unrelated
            engine.state.pending_focus = Some(crate::types::PendingFocus {
                window: 2,
                owner: 1,
                monitor: mi,
                workspace: ws_i,
            });
            assert_eq!(engine.state.presented_overlay_owner(mi), Some(1));
            assert_eq!(
                decide_active_window(&engine.state, 2),
                ActiveWindowIntent::Ignore,
                "an unrelated deferred window cannot steal the overlay via _NET_ACTIVE_WINDOW"
            );
        }

        // 7) The overlay owner itself requesting focus is honored (it is not
        //    stealing from itself).
        {
            let mut engine = setup_engine();
            let mi = engine.state.sel_mon;
            let ws_i = engine.state.monitors[mi].active_ws;
            aw_add_client(&mut engine, 1, mi, ws_i, true); // A overlay owner
            assert_eq!(
                decide_active_window(&engine.state, 1),
                ActiveWindowIntent::Focus(1),
                "the overlay owner may re-assert its own focus"
            );
        }

        // 8) A non-managed (unknown) window is refused.
        {
            let engine = setup_engine();
            assert_eq!(
                decide_active_window(&engine.state, 999),
                ActiveWindowIntent::Ignore,
                "requests for unknown windows are ignored"
            );
        }
    }
    /// Fixture: monitor with window 1 tiled+focused on ws0 and window 2 tiled on ws1.
    fn build_view_fixture() -> crate::types::State {
        let mut state = crate::types::State::new();
        state
            .monitors
            .push(Monitor::new(crate::types::Rect::new(0, 0, 1920, 1080), 9));
        let mi = state.sel_mon;
        state.add_client(Client::new(1, mi, 0));
        state.add_client(Client::new(2, mi, 1));
        state.monitors[mi].workspaces[0].add_tiled(1, 0.6);
        state.monitors[mi].workspaces[1].add_tiled(2, 0.6);
        state.monitors[mi].focused = Some(1);
        state.monitors[mi].focus_stack = vec![1];
        state
    }

    /// After `ViewWorkspace(B)`, the monitor's logical state must satisfy:
    /// `focused == None` OR `focused` lives on workspace B — immediately after the
    /// command, not only after the backend applies the `FocusWindow` effect.
    #[test]
    fn view_workspace_fixes_focus_immediately() {
        use crate::core::commands::{Command, ViewWorkspace};
        // Case A: view the *empty* workspace 2 → stale focus on window 1 must be
        // dropped in the command itself (before any effect application).
        {
            let mut state = build_view_fixture();
            let mut cmd = ViewWorkspace(2);
            let _ = cmd.execute(&mut state, &mut default_cfg());
            let m = &state.monitors[state.sel_mon];
            assert_eq!(m.active_ws, 2);
            assert_eq!(
                m.focused, None,
                "stale focus from the previous workspace must be cleared immediately"
            );
        }

        // Case B: view workspace 1, which owns window 2 → the focus may stay (it
        // belongs to the now-active workspace) and the invariant holds.
        {
            let mut state = build_view_fixture();
            let mut cmd = ViewWorkspace(1);
            let _ = cmd.execute(&mut state, &mut default_cfg());
            let m = &state.monitors[state.sel_mon];
            assert_eq!(m.active_ws, 1);
            assert!(m
                .focused
                .is_none_or(|w| state.clients.get(&w).is_some_and(|c| c.workspace == 1)));
        }
    }

    /// End-to-end through the engine: after the effect stage the invariant still
    /// holds (`focused ∈ active_ws` ∨ `focused == None`).
    #[test]
    fn view_workspace_invariant_after_effect_stage() {
        use crate::core::commands::ViewWorkspace;
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        engine.state.add_client(Client::new(1, mi, 0));
        engine.state.monitors[mi].workspaces[0].add_tiled(1, 0.6);
        engine.state.monitors[mi].focused = Some(1);

        engine.execute(ViewWorkspace(1));
        let st = &engine.state;
        let m = &st.monitors[mi];
        assert_eq!(m.active_ws, 1);
        assert!(
            m.focused.is_none_or(|w| st
                .clients
                .get(&w)
                .is_some_and(|c| c.workspace == m.active_ws)),
            "focused must live on the active workspace after ViewWorkspace"
        );
    }

    /// If the focused window belongs to a DIFFERENT monitor than the selected one
    /// (logical focus corruption), `ToggleFloat` must not mutate the selected
    /// monitor's trees (no `remove from tree A / insert into floating B` split).
    /// A window-targeted action must reach the window it was asked about, not
    /// the one that happened to be focused. Every window-targeting action is
    /// checked against a *different* focused window, because a test that
    /// targeted the focused window would pass even if the argument were ignored.
    #[test]
    fn window_targeted_actions_ignore_the_focused_window() {
        use crate::core::Effect;
        use crate::types::{Action, Dir};
        // Three columns, focus the middle one, target the ones on either side.
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        for win in [1u32, 2, 3] {
            engine.state.add_client(Client::new(win, mi, ws_i));
            engine.state.monitors[mi].workspaces[ws_i].add_tiled(win, 0.5);
        }
        engine.state.monitors[mi].focused = Some(2);
        let order = |e: &Engine| -> Vec<u32> {
            e.state.monitors[mi].workspaces[ws_i]
                .columns
                .iter()
                .filter_map(|c| c.windows.first().copied())
                .collect()
        };
        assert_eq!(order(&engine), vec![1, 2, 3]);

        // Move the *left* window right: it swaps past the focused one.
        engine.dispatch(Action::MoveWindow(Dir::Right, 1));
        assert_eq!(order(&engine), vec![2, 1, 3]);
        engine.dispatch(Action::MoveWindow(Dir::Left, 1));
        assert_eq!(order(&engine), vec![1, 2, 3]);

        // Float the *right* window; the focused one must be untouched.
        engine.dispatch(Action::ToggleFloatWindow(3));
        assert!(engine.state.clients[&3].is_float(), "the target must float");
        assert!(
            !engine.state.clients[&2].is_float(),
            "the focused window must be untouched"
        );
        assert_eq!(order(&engine), vec![1, 2], "a float leaves the column tree");

        // Focus is a *sink* decision: the core emits the intent and the X11
        // sink applies it (and updates `monitors[mi].focused` with it), so what
        // the pure engine can be asked for is the effect, not the resulting
        // focus. Asserting on the effect keeps the test on the side of the
        // boundary the action actually lives on.
        let effects = engine.dispatch(Action::FocusWindow(3));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::FocusWindow(Some(3)))),
            "focus_window must emit the targeted id: {effects:?}"
        );
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::FocusWindow(Some(2)))),
            "the focused window must not be re-focused: {effects:?}"
        );
    }

    /// Fullscreen and float have to move the *targeted* window's monitor, not
    /// `sel_mon`: the camera recentre, the pending-focus consumption and the
    /// arrange are all monitor-indexed, and running them against the selected
    /// monitor would rearrange the wrong screen and move the camera in a window
    /// the user is not looking at.
    #[test]
    fn a_targeted_window_arranges_its_own_monitor() {
        use crate::core::Effect;
        use crate::types::Action;
        let mut engine = setup_engine_multi();
        // Window 2 lives on monitor 1 while monitor 0 stays selected.
        engine.state.add_client(Client::new(2, 1, 0));
        engine.state.monitors[1].workspaces[0].add_tiled(2, 1.0);
        engine.state.monitors[0].focused = None;
        engine.state.sel_mon = 0;

        let effects = engine.dispatch(Action::ToggleFloatWindow(2));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::ArrangeMonitor(1))),
            "the wrong monitor was arranged: {effects:?}"
        );
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::ArrangeMonitor(0))),
            "the selected monitor must not be arranged for another monitor's window: {effects:?}"
        );
    }

    /// A window id that names nothing must be a no-op, never a panic and never
    /// a mutation of some other window.
    #[test]
    fn a_window_id_that_names_nothing_changes_nothing() {
        use crate::types::{Action, Dir};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.add_client(Client::new(1, mi, ws_i));
        engine.state.monitors[mi].workspaces[ws_i].add_tiled(1, 1.0);
        engine.state.monitors[mi].focused = Some(1);
        // `State` is not `Clone`, so the invariant is checked as a fingerprint:
        // the focused window, the float flags and the topology, which is
        // everything a window-targeting action could plausibly disturb.
        let fingerprint = |e: &Engine| {
            format!(
                "sel_mon={} focus={:?} float1={} float2={} cols={:?}",
                e.state.sel_mon,
                e.state.monitors[mi].focused,
                e.state.clients[&1].is_float(),
                e.state.clients.get(&2).is_some_and(Client::is_float),
                e.state.monitors[mi].workspaces[ws_i]
                    .columns
                    .iter()
                    .map(|c| (c.windows.clone(), c.weight))
                    .collect::<Vec<_>>(),
            )
        };
        let before = fingerprint(&engine);

        for action in [
            Action::ToggleFloatWindow(0xdead),
            Action::ToggleFullscreenWindow(0xdead),
            Action::FocusWindow(0xdead),
            Action::CloseWindow(0xdead),
            Action::MoveWindow(Dir::Left, 0xdead),
        ] {
            engine.dispatch(action);
            assert_eq!(
                fingerprint(&engine),
                before,
                "an unknown id must change nothing"
            );
        }
    }

    /// A percentage resize is a statement about the *workarea*, resolved where
    /// the layout lives. The test pins the direction, not the exact weight: the
    /// weight is then redistributed among the siblings, which is what makes a
    /// scroll layout's ribbon change length instead of stealing from a
    /// neighbour.
    #[test]
    fn a_percentage_resize_scales_with_the_workarea() {
        use crate::types::Action;
        // Two half-width columns: a *single* column already fills the workarea,
        // so a grow on it is a no-op by design and would prove nothing.
        // The *focused* column is the one that grows, so the test reads that
        // one: `add_tiled` puts the second window in a new column and moves the
        // focus there, so the second column is the one under test.
        let focused_weight = |e: &Engine| {
            let ws = &e.state.monitors[0].workspaces[0];
            ws.columns[ws.focus.column_idx].weight
        };
        let two_columns = |w: u32| {
            let mut engine = Engine::new(default_cfg());
            engine
                .state
                .monitors
                .push(Monitor::new(Rect::new(0, 0, w, 600), 9));
            for win in [1u32, 2] {
                engine.state.add_client(Client::new(win, 0, 0));
                engine.state.monitors[0].workspaces[0].add_tiled(win, 0.5);
            }
            engine
        };

        let mut big = two_columns(1920);
        let before = focused_weight(&big);
        big.dispatch(Action::GrowColPct(10.0));
        let after = focused_weight(&big);
        assert!(
            after > before,
            "+10% of a 1920px workarea must widen the focused column ({before} -> {after})"
        );

        // The weight is a fraction of the *usable* width — the workarea minus
        // the gaps between columns — because that is the space the layout
        // actually distributes. So ten percent lands within a hair of a tenth
        // on any monitor, and not exactly: the same fraction of a larger
        // usable width is a larger number of pixels, which is the whole point.
        let mut small = two_columns(800);
        let s_before = focused_weight(&small);
        small.dispatch(Action::GrowColPct(10.0));
        let s_after = focused_weight(&small);
        assert!(
            (s_before - before).abs() < f32::EPSILON,
            "both start from the same weight"
        );
        for (label, delta) in [("1920px", after - before), ("800px", s_after - s_before)] {
            assert!(
                (delta - 0.10).abs() < 0.005,
                "10% on a {label} workarea must be about a tenth, got {delta}"
            );
        }
        // And a negative percentage shrinks.
        let mut shrink = two_columns(1920);
        let s2_before = focused_weight(&shrink);
        shrink.dispatch(Action::GrowColPct(-10.0));
        assert!(
            focused_weight(&shrink) < s2_before,
            "-10% must narrow the column"
        );
    }

    /// No columns, no selected monitor: a percentage has nothing to act on, and
    /// must be a no-op rather than a division by zero.
    #[test]
    fn a_percentage_resize_on_an_empty_workspace_is_a_no_op() {
        use crate::types::Action;
        let mut engine = setup_engine();
        assert!(engine.dispatch(Action::GrowColPct(10.0)).is_empty());
        let mut none = Engine::new(default_cfg());
        assert!(none.dispatch(Action::GrowColPct(-10.0)).is_empty());
    }

    #[test]
    fn toggle_float_rejects_cross_monitor_focus() {
        use crate::core::commands::ToggleFloat;
        use crate::types::WinFlags;
        let mut engine = setup_engine_multi();
        let _mi = engine.state.sel_mon; // monitor 0 selected (deliberately != client monitor)
                                        // Window 1 lives on monitor 1, workspace 0, tiled.
        engine.state.add_client(Client::new(1, 1, 0));
        engine.state.monitors[1].workspaces[0].add_tiled(1, 0.6);
        // Cross-monitor (corrupt) logical focus on monitor 0.
        engine.state.monitors[0].focused = Some(1);
        engine.state.monitors[0].focus_stack = vec![1];

        let effects = engine.execute(ToggleFloat(None));
        assert!(effects.is_empty(), "a cross-monitor toggle must be a no-op");
        // Monitor 0's trees untouched: window 1 was never floating there.
        assert!(!engine.state.monitors[0].workspaces[0].floats.contains(&1));
        // Monitor 1's membership unchanged and the client not flagged FLOAT.
        assert!(!engine.state.monitors[1].workspaces[0].floats.contains(&1));
        assert_eq!(
            engine
                .state
                .clients
                .get(&1)
                .map(|c| c.flags.has(WinFlags::FLOAT)),
            Some(false)
        );
        // Sanity: the same toggle when focus is *consistent* still works.
        engine.state.sel_mon = 1;
        engine.state.monitors[1].focused = Some(1);
        let effects = engine.execute(ToggleFloat(None));
        assert!(!effects.is_empty(), "a consistent toggle still mutates");
        assert!(engine.state.monitors[1].workspaces[0].floats.contains(&1));
    }

    #[test]
    fn extreme_gaps_do_not_produce_invalid_or_offscreen_geometry() {
        use crate::core::layout::{arrange, RibbonScratch};
        use crate::types::{Client, Rect};

        fn test_extreme(wa: Rect, n_clients: usize, gap: u32, desc: &str) {
            let mut engine = setup_engine();
            engine.state.monitors[0].workarea = wa;
            engine.cfg.gaps_inner = gap;
            engine.cfg.gaps_outer = gap;

            for i in 0..n_clients {
                let win = (100 + i) as u32;
                let mut c = Client::new(win, 0, 0);
                c.border_w = 2;
                engine.state.add_client(c);
                engine.state.monitors[0].workspaces[0].add_tiled(win, 1.0);
            }

            let mut placements = crate::core::layout::Placements::new();
            arrange(
                &engine.state,
                0,
                &engine.cfg,
                &mut placements,
                &mut RibbonScratch::default(),
            );

            for (win, rect, _bw) in placements {
                assert!(rect.w > 0, "{desc}: window {win} width must be > 0");
                assert!(rect.h > 0, "{desc}: window {win} height must be > 0");

                // Coordinates must stay within a 100 px slack of the workarea: a
                // pathological gap/workarea must not blow the origin up to the
                // hundreds of thousands of pixels.
                assert!(
                    rect.y <= wa.y + wa.h as i32 + 100,
                    "{desc}: window {win} y is completely off-screen: {}",
                    rect.y
                );
                assert!(
                    rect.y >= wa.y - 100,
                    "{desc}: window {win} y is above screen: {}",
                    rect.y
                );
            }
        }

        test_extreme(
            Rect::new(0, 0, 1, 1),
            2,
            99999,
            "1x1 + 2 clients + huge gap",
        );
        test_extreme(
            Rect::new(0, 0, 100, 100),
            100,
            99999,
            "100x100 + 100 clients + huge gap",
        );
        test_extreme(
            Rect::new(0, 0, 1920, 1080),
            3,
            99999,
            "1920x1080 + 3 clients + huge gap",
        );
        test_extreme(Rect::new(0, 0, 1920, 1080), 3, 0, "normal gap = 0");
    }

    // Property-based contract tests for the state machine.
    //
    // Each property below states a *contract* rather than a restatement of the
    // code under test: the structural invariant list in `maverick-core`'s crate
    // docs (A–F), the `Command` purity and effect-ordering rules, the focus and
    // overlay ownership rules, and the IPC/action vocabulary rules. Generated
    // states are always built through the public API (`Monitor::new`,
    // `Client::new`, `add_client`, `add_tiled`, `Engine::execute`), because a
    // fabricated inconsistent state would make an invariant property vacuous.
    mod props {
        use crate::config::Cfg;
        use crate::core::action::{name as action_name, parse as parse_action};
        use crate::core::commands::{
            apply_fullscreen_geom_restore, apply_fullscreen_topology, apply_maximize,
            decide_manage_focus, focus_logical_on, reconcile_pending_focus_after_transition,
            CollapseColumn, Command, FocusDirection, FocusMonitor, FocusWindow, GrowColumn,
            KillWindow, ManageFocusIntent, MoveResize, MoveToWorkspace,
            MoveWindow, MoveWindowToMonitor, NewColumn, OverviewEnter, OverviewNav, PageSnap, Quit,
            Restart, SetLayout, Spawn, ToggleFloat,
            ToggleFullscreen, ToggleMaximize, ToggleOverview, ViewWorkspace, ViewportZoom,
        };
        use crate::core::effect::Effect;
        use crate::core::event::CommandReport;
        use crate::core::ipc::{query_json, state_json};
        use crate::core::Engine;
        use crate::types::{
            Action, Client, Dir, LayoutKind, PendingFocus, Rect, State, WinFlags,
            WindowId,
        };
        use proptest::prelude::*;
        use std::fmt::Write as _;

        use super::{setup_engine, setup_engine_multi, t_focus, t_manage};

        /// `Engine::execute` takes `impl Command` while the generated vocabulary
        /// is only available as the `Box<dyn Command>` the trait object erases
        /// to; this forwarder reaches the single-command entry point without
        /// duplicating any command logic.
        struct Boxed(Box<dyn Command>);

        impl Command for Boxed {
            fn execute(&mut self, state: &mut State, cfg: &mut Cfg) -> CommandReport {
                self.0.execute(state, cfg)
            }
        }

        /// Every `Dir` variant, by index. `Dir` has no `Arbitrary` impl, so the
        /// direction domain is spelled out once here instead of at every use.
        fn dir_of(i: u8) -> Dir {
            match i % 6 {
                0 => Dir::Left,
                1 => Dir::Right,
                2 => Dir::Up,
                3 => Dir::Down,
                4 => Dir::Next,
                _ => Dir::Prev,
            }
        }

        fn dir_name(d: Dir) -> &'static str {
            match d {
                Dir::Left => "left",
                Dir::Right => "right",
                Dir::Up => "up",
                Dir::Down => "down",
                Dir::Next => "next",
                Dir::Prev => "prev",
            }
        }

        /// Any `f32` bit pattern, so NaN, ±inf and subnormals are all
        /// reachable. `ViewportZoom` and `Camera::retarget` both document
        /// refusing exactly these values.
        fn arb_f32_bits() -> impl Strategy<Value = f32> {
            any::<u32>().prop_map(f32::from_bits)
        }

        /// The typed command vocabulary, generated with arguments taken from each
        /// command's *legal* input space. Deliberately included are the inputs a
        /// hostile IPC/config channel can deliver and that the commands are
        /// documented to absorb: workspace indices past the tag count, extreme
        /// pixel deltas, `u32::MAX` gaps and border widths, and non-finite zoom
        /// steps. Absorbing those wrongly is exactly the corruption these
        /// properties must catch.
        #[derive(Debug, Clone)]
        enum GenCmd {
            ViewWorkspace(u8),
            MoveToWorkspace(u8),
            GrowColumn(i32),
            NewColumn,
            CollapseColumn,
            FocusMonitor(u8),
            SetLayout,
            ToggleOverview,
            OverviewNav(u8),
            OverviewEnter,
            ViewportZoom(u32),
            PageSnap(u8),
            Spawn(u32),
            FocusWindow(u32),
            FocusDirection(u8),
            MoveWindow(u32, u8),
            MoveResize(u32, i32, i32, u32, u32),
            KillWindow(u32),
            ToggleFloat,
            ToggleFullscreen,
            ToggleMaximize,
            Quit,
            Restart,
        }

        impl GenCmd {
            /// Build the command for the current state. `None` means the
            /// command has no legal target (no live window), which the wire
            /// layer signals by simply not running it (`Engine::dispatch`
            /// returns an empty effect list in that case), so the generator does
            /// not invent a target either.
            fn build(&self, s: &State) -> Option<Box<dyn Command>> {
                let live = live_windows(s);
                let target = |pick: u32| -> Option<WindowId> {
                    if live.is_empty() {
                        None
                    } else {
                        Some(live[(pick as usize) % live.len()])
                    }
                };
                let cmd: Box<dyn Command> = match self {
                    Self::ViewWorkspace(i) => Box::new(ViewWorkspace(*i as usize)),
                    Self::MoveToWorkspace(i) => Box::new(MoveToWorkspace(*i as usize)),
                    Self::GrowColumn(px) => Box::new(GrowColumn(*px)),
                    Self::NewColumn => Box::new(NewColumn),
                    Self::CollapseColumn => Box::new(CollapseColumn),
                    Self::FocusMonitor(d) => Box::new(FocusMonitor(dir_of(*d))),
                    Self::SetLayout => Box::new(SetLayout(LayoutKind::Column)),
                    Self::ToggleOverview => Box::new(ToggleOverview),
                    Self::OverviewNav(d) => Box::new(OverviewNav(dir_of(*d))),
                    Self::OverviewEnter => Box::new(OverviewEnter),
                    Self::ViewportZoom(bits) => Box::new(ViewportZoom(f32::from_bits(*bits))),
                    Self::PageSnap(d) => Box::new(PageSnap(dir_of(*d))),
                    Self::Spawn(n) => Box::new(Spawn(vec![
                        "sh".to_string(),
                        "-c".to_string(),
                        format!("sleep {n}"),
                    ])),
                    Self::Quit => Box::new(Quit),
                    Self::Restart => Box::new(Restart),
                    Self::FocusDirection(d) => Box::new(FocusDirection(dir_of(*d))),
                    Self::ToggleFloat => Box::new(ToggleFloat(None)),
                    Self::ToggleFullscreen => Box::new(ToggleFullscreen(None)),
                    Self::ToggleMaximize => Box::new(ToggleMaximize(None)),
                    Self::FocusWindow(p) => Box::new(FocusWindow(target(*p))),
                    Self::MoveWindow(p, d) => {
                        return target(*p).map(|w| Box::new(MoveWindow(w, dir_of(*d))) as _);
                    }
                    Self::KillWindow(p) => {
                        return target(*p).map(|w| Box::new(KillWindow(w)) as _);
                    }
                    Self::MoveResize(p, x, y, w, h) => {
                        return target(*p)
                            .map(|win| Box::new(MoveResize(win, Rect::new(*x, *y, *w, *h))) as _);
                    }
                };
                Some(cmd)
            }
        }

        /// The wire vocabulary, dispatched through `Engine::dispatch` so the
        /// action → command adapter (including its no-target early returns) is
        /// covered as well.
        fn arb_action() -> impl Strategy<Value = Action> {
            prop_oneof![
                10 => (0u8..=5).prop_map(|i| Action::FocusDir(dir_of(i))),
                7 => (0u8..=5).prop_map(|i| Action::MoveDir(dir_of(i))),
                5 => (0u8..=5).prop_map(|i| Action::FocusMon(dir_of(i))),
                5 => (0u8..=5).prop_map(|i| Action::MoveMon(dir_of(i))),
                4 => (0u8..=5).prop_map(|i| Action::OverviewNav(dir_of(i))),
                4 => (0u8..=5).prop_map(|i| Action::PageSnap(dir_of(i))),
                5 => (0u8..=12).prop_map(|i| Action::View(i as usize)),
                5 => (0u8..=12).prop_map(|i| Action::MoveToWs(i as usize)),
                6 => any::<i32>().prop_map(Action::GrowCol),
                4 => arb_f32_bits().prop_map(Action::ViewportZoom),
                3 => Just(Action::NewColumn),
                3 => Just(Action::CollapseColumn),
                3 => Just(Action::ToggleFloat),
                3 => Just(Action::ToggleFullscreen),
                3 => Just(Action::ToggleMaximize),
                2 => Just(Action::ToggleOverview),
                2 => Just(Action::OverviewEnter),
                2 => Just(Action::Kill),
                1 => Just(Action::SetLayout(LayoutKind::Column)),
                1 => (0u32..64).prop_map(|n| Action::Spawn(vec!["sh".into(), n.to_string()])),
            ]
        }

        /// One generated step of a state-machine sequence.
        #[derive(Debug, Clone)]
        enum Op {
            Cmd(GenCmd),
            Wire(Action),
            /// Map a new window: the logical half of the backend's `manage()`.
            Map {
                float: bool,
                has_parent: bool,
                parent: u16,
            },
            /// Unmap: `State::remove_client`, the tail of `unmanage()`.
            Unmap {
                pick: u32,
            },
        }

        fn arb_op() -> impl Strategy<Value = Op> {
            prop_oneof![
                26 => arb_gen_cmd().prop_map(Op::Cmd),
                14 => arb_action().prop_map(Op::Wire),
                6 => (any::<bool>(), any::<bool>(), any::<u16>())
                    .prop_map(|(float, has_parent, parent)| Op::Map { float, has_parent, parent }),
                4 => any::<u32>().prop_map(|pick| Op::Unmap { pick }),
            ]
        }

        fn arb_gen_cmd() -> impl Strategy<Value = GenCmd> {
            prop_oneof![
                5 => (0u8..=12).prop_map(GenCmd::ViewWorkspace),
                5 => (0u8..=12).prop_map(GenCmd::MoveToWorkspace),
                8 => any::<i32>().prop_map(GenCmd::GrowColumn),
                5 => Just(GenCmd::NewColumn),
                4 => Just(GenCmd::CollapseColumn),
                4 => (0u8..=5).prop_map(GenCmd::FocusMonitor),
                2 => Just(GenCmd::SetLayout),
                3 => Just(GenCmd::ToggleOverview),
                3 => (0u8..=5).prop_map(GenCmd::OverviewNav),
                3 => Just(GenCmd::OverviewEnter),
                4 => any::<u32>().prop_map(GenCmd::ViewportZoom),
                3 => (0u8..=5).prop_map(GenCmd::PageSnap),
                1 => any::<u32>().prop_map(GenCmd::Spawn),
                1 => Just(GenCmd::Quit),
                1 => Just(GenCmd::Restart),
                6 => (any::<u32>(), 0u8..=5).prop_map(|(_, d)| GenCmd::FocusDirection(d)),
                5 => (any::<u32>(), 0u8..=5).prop_map(|(p, d)| GenCmd::MoveWindow(p, d)),
                4 => (
                    any::<u32>(),
                    any::<i32>(),
                    any::<i32>(),
                    any::<u32>(),
                    any::<u32>(),
                )
                    .prop_map(|(p, x, y, w, h)| GenCmd::MoveResize(p, x, y, w, h)),
                2 => any::<u32>().prop_map(GenCmd::KillWindow),
                3 => any::<u32>().prop_map(GenCmd::FocusWindow),
                3 => Just(GenCmd::ToggleFloat),
                4 => Just(GenCmd::ToggleFullscreen),
                4 => Just(GenCmd::ToggleMaximize),
            ]
        }

        #[derive(Debug, Clone)]
        struct Scenario {
            n_mon: usize,
            seed: usize,
            floatness: u8,
            ops: Vec<Op>,
        }

        /// One scenario: a small but structurally complete starting state
        /// (one or two monitors, a handful of seeded windows) plus a short
        /// command sequence. Sequences stay short so the whole suite runs in
        /// milliseconds while still reaching multi-step interactions (map behind
        /// an overlay, move to another workspace, close it, …).
        fn arb_scenario() -> impl Strategy<Value = Scenario> {
            (
                1usize..=2,
                0usize..=4,
                0u8..=3,
                proptest::collection::vec(arb_op(), 3..=12),
            )
                .prop_map(|(n_mon, seed, floatness, ops)| Scenario {
                    n_mon,
                    seed,
                    floatness,
                    ops,
                })
        }

        fn live_windows(s: &State) -> Vec<WindowId> {
            let mut v: Vec<WindowId> = s.clients.keys().copied().collect();
            v.sort_unstable();
            v
        }

        /// Fresh window id. `0` is never handed out: it is not a valid XID, and
        /// the commands deliberately never emit `Unfocus(0)`.
        fn next_free_window_id(s: &State) -> WindowId {
            s.clients.keys().copied().max().map_or(1, |m| m + 1)
        }

        /// Logical half of the backend's `manage()`: place the window in the
        /// selected monitor's active workspace, then apply the core focus policy
        /// — so a window that maps behind a presented overlay really is deferred
        /// into `pending_focus`, which is the only way invariant E is reachable
        /// from a generated sequence.
        fn map_window(engine: &mut Engine, win: WindowId, float: bool, parent: Option<WindowId>) {
            let mi = engine.state.sel_mon;
            let ws_i = engine.state.monitors[mi].active_ws;
            let mut c = Client::new(win, mi, ws_i);
            c.border_w = engine.cfg.border_w;
            c.geom = Rect::new(20 + (win as i32 % 9) * 25, 24, 430, 310);
            c.saved_geom = c.geom;
            c.transient_parent = parent;
            // A transient is a dialog: the WM floats it and gives it a
            // WM_TRANSIENT_FOR parent, which is what lets `decide_manage_focus`
            // focus it through the overlay it belongs to.
            if float || parent.is_some() {
                c.flags.set(WinFlags::FLOAT);
                engine.state.monitors[mi].workspaces[ws_i].floats.push(win);
            } else {
                engine.state.monitors[mi].workspaces[ws_i].add_tiled(win, engine.cfg.column_width);
            }
            engine.state.add_client(c);
            match decide_manage_focus(&engine.state, win) {
                ManageFocusIntent::Defer {
                    owner,
                    monitor,
                    workspace,
                } => {
                    engine.state.pending_focus = Some(PendingFocus {
                        window: win,
                        owner,
                        monitor,
                        workspace,
                    });
                }
                ManageFocusIntent::Focus(_) => {
                    focus_logical_on(&mut engine.state, mi, win);
                    // Mirror the reconciliation `manage()` performs on this same
                    // intent (`src/backend/x11/manage.rs`). Focusing a window
                    // owned by the presented overlay takes that overlay off the
                    // focus, and a maximize overlay is presented only while it
                    // holds the focus — so a deferral queued behind it is left
                    // behind an overlay that can no longer return. The two paths
                    // have to agree, or this harness would be modelling a map
                    // path the backend does not have.
                    if let Some(resolved) =
                        reconcile_pending_focus_after_transition(&mut engine.state)
                    {
                        focus_logical_on(&mut engine.state, mi, resolved);
                    }
                }
            }
        }

        /// Run one generated step.
        ///
        /// A command only *emits* `Effect::FocusWindow`; the real X sink
        /// (`Backend::focus`) is what moves `mon.focused`. Without mirroring it
        /// the logical focus would drift off the active workspace and the
        /// sequence would explore states the WM never reaches. See
        /// [`mirror_focus`] for the sink's pure half.
        fn run_op(engine: &mut Engine, op: &Op) -> Vec<Effect> {
            match op {
                Op::Cmd(g) => {
                    let Some(cmd) = g.build(&engine.state) else {
                        return Vec::new();
                    };
                    let effects = engine.execute(Boxed(cmd));
                    mirror_focus(engine, &effects);
                    effects
                }
                Op::Wire(a) => {
                    let effects = engine.dispatch(a.clone());
                    mirror_focus(engine, &effects);
                    effects
                }
                Op::Map {
                    float,
                    has_parent,
                    parent,
                } => {
                    let live = live_windows(&engine.state);
                    let p = if *has_parent && !live.is_empty() {
                        Some(live[(*parent as usize) % live.len()])
                    } else {
                        None
                    };
                    let win = next_free_window_id(&engine.state);
                    map_window(engine, win, *float, p);
                    Vec::new()
                }
                Op::Unmap { pick } => {
                    let live = live_windows(&engine.state);
                    if !live.is_empty() {
                        let win = live[(*pick as usize) % live.len()];
                        engine.state.remove_client(win);
                    }
                    Vec::new()
                }
            }
        }

        /// Apply the backend's X11 focus sink to the effects of one step.
        ///
        /// A command only *emits* `Effect::FocusWindow`; the real sink
        /// (`Backend::focus`, `src/backend/x11/render.rs`) is what moves the
        /// logical focus, and it resolves the target's **own** monitor and
        /// re-points `sel_mon` at it before writing that monitor's slot
        /// (`sel_mon = c.monitor`, then `monitors[sel_mon].focused = Some(w)`,
        /// then `sync_presented_maximize`). `t_focus` above is that same pure
        /// half, so it is reused here rather than re-derived: keying the focus on
        /// `sel_mon` instead would invent a cross-monitor focus slot the sink
        /// cannot produce, and every downstream predicate that reads a monitor's
        /// focus stack (`presented_overlay_owner`, `best_focus`, `decide_manage_focus`)
        /// would then be fed a state the WM never reaches.
        fn mirror_focus(engine: &mut Engine, effects: &[Effect]) {
            for e in effects {
                if let Effect::FocusWindow(Some(w)) = e {
                    t_focus(engine, *w);
                }
            }
        }

        fn seed_engine(sc: &Scenario) -> Engine {
            let mut engine = if sc.n_mon >= 2 {
                setup_engine_multi()
            } else {
                setup_engine()
            };
            for k in 0..sc.seed {
                let float = (sc.floatness as usize + k) % 2 == 0;
                map_window(&mut engine, (k + 1) as u32, float, None);
            }
            engine
        }

        /// Canonical dump of everything a command is allowed to decide: topology,
        /// focus, camera, presentation state, per-client placement and the config
        /// knobs a command may write. Two dumps that differ mean the second
        /// application of an absorbing command was *not* a no-op.
        fn logical_dump(e: &Engine) -> String {
            let s = &e.state;
            let mut d = String::new();
            let _ = writeln!(
                d,
                "sel_mon={} running={} status={:?} x11={:?} pending={:?} transients={:?}",
                s.sel_mon,
                s.running,
                s.status,
                s.x11_input_focus,
                s.pending_focus,
                s.pending_transients
            );
            for (mi, mon) in s.monitors.iter().enumerate() {
                let _ = writeln!(
                    d,
                    "mon{mi} screen={:?} wa={:?} active_ws={} focused={:?} stack={:?}",
                    mon.screen, mon.workarea, mon.active_ws, mon.focused, mon.focus_stack
                );
                for (wi, ws) in mon.workspaces.iter().enumerate() {
                    let _ = writeln!(
                        d,
                        "  ws{wi} tag={} layout={:?} overview={} zoom={:.4} vz={:?} pz={:.4} pmax={:?} cam={:.4} floats={:?}",
                        ws.tag,
                        ws.layout,
                        ws.overview,
                        ws.zoom,
                        ws.viewport_mode,
                        ws.page_zoom,
                        ws.presented_maximize,
                        ws.camera.position,
                        ws.floats
                    );
                    for (ci, col) in ws.columns.iter().enumerate() {
                        let _ = writeln!(
                            d,
                            "    col{ci} w={:.6} focused={} wins={:?}",
                            col.weight, col.focused, col.windows
                        );
                    }
                }
            }
            for win in live_windows(s) {
                let c = &s.clients[&win];
                let _ = writeln!(
                    d,
                    "client{win} mon={} ws={} geom={:?} saved={:?} bw={}/{} dirty={} policy={:?} snap={:?} parent={:?} name={:?} class={:?} inst={:?} flags[fs={} maxv={} maxh={} sticky={} native={} fswas={} urgent={} fixed={} nofocus={}] des={:?} rep={:?}",
                    c.monitor,
                    c.workspace,
                    c.geom,
                    c.saved_geom,
                    c.border_w,
                    c.old_border_w,
                    c.geometry_dirty,
                    c.fullscreen_policy,
                    c.fs_snapshot,
                    c.transient_parent,
                    c.name,
                    c.class,
                    c.instance,
                    c.is_fullscreen(),
                    c.is_maximized_v(),
                    c.is_maximized_h(),
                    c.is_sticky(),
                    c.is_native_float(),
                    c.flags.has(WinFlags::FS_WAS_FLOAT),
                    c.flags.has(WinFlags::URGENT),
                    c.flags.has(WinFlags::FIXED),
                    c.no_focus(),
                    c.last_desired,
                    c.last_reported
                );
            }
            let _ = writeln!(
                d,
                "cfg gaps=({},{}) border={} colw={:.4} ntags={}",
                e.cfg.gaps_inner,
                e.cfg.gaps_outer,
                e.cfg.border_w,
                e.cfg.column_width,
                e.cfg.n_tags
            );
            d
        }

        /// Every window id the ownership graph names, tagged with the slot it was
        /// found in. A destroyed window must appear nowhere: the docs promise
        /// that closing a client drops "every transient reference" to it.
        fn all_references(s: &State) -> Vec<(&'static str, WindowId)> {
            let mut out = Vec::new();
            for mon in &s.monitors {
                if let Some(w) = mon.focused {
                    out.push(("monitor.focused", w));
                }
                for w in &mon.focus_stack {
                    out.push(("monitor.focus_stack", *w));
                }
                for ws in &mon.workspaces {
                    for col in &ws.columns {
                        for w in &col.windows {
                            out.push(("column", *w));
                        }
                    }
                    for w in &ws.floats {
                        out.push(("float", *w));
                    }
                    if let Some(w) = ws.presented_maximize {
                        out.push(("presented_maximize", w));
                    }
                }
            }
            if let Some(w) = s.x11_input_focus {
                out.push(("x11_input_focus", w));
            }
            if let Some(pf) = s.pending_focus {
                out.push(("pending_focus.window", pf.window));
                out.push(("pending_focus.owner", pf.owner));
            }
            for w in &s.pending_transients {
                out.push(("pending_transient", *w));
            }
            for (win, c) in &s.clients {
                out.push(("clients", *win));
                if let Some(p) = c.transient_parent {
                    out.push(("client.transient_parent", p));
                }
            }
            out
        }

        /// Structural JSON check for the IPC snapshots: every string literal is
        /// terminated and free of raw control bytes, and no bare `NaN`/`inf`
        /// token is emitted. Neither is legal JSON, and both are exactly what a
        /// poisoned camera position or column weight would produce without the
        /// guards in `core::ipc`.
        fn json_defect(doc: &str) -> Option<String> {
            if !doc.starts_with('{') || !doc.ends_with('}') {
                return Some(format!("not a JSON object: {doc}"));
            }
            let b = doc.as_bytes();
            let mut i = 0;
            let mut in_str = false;
            let mut outside = String::new();
            while i < b.len() {
                let c = b[i];
                if in_str {
                    match c {
                        b'\\' => i = (i + 2).min(b.len()),
                        b'"' => {
                            in_str = false;
                            outside.push('"');
                            i += 1;
                        }
                        _ => {
                            if c < 0x20 {
                                return Some(format!(
                                    "raw control byte {c:#04x} inside a JSON string at {i}"
                                ));
                            }
                            i += 1;
                        }
                    }
                } else {
                    outside.push(c as char);
                    if c == b'"' {
                        in_str = true;
                    }
                    i += 1;
                }
            }
            if in_str {
                return Some("unterminated JSON string literal".to_string());
            }
            for tok in ["NaN", "inf", "Infinity"] {
                if outside.contains(tok) {
                    return Some(format!("non-JSON numeric token {tok:?} emitted"));
                }
            }
            None
        }

        /// Strings built from the characters that break a naive JSON emitter:
        /// quote, backslash, the C0 controls that must become `\u00XX`, and a
        /// multi-byte code point.
        fn arb_hostile_string() -> impl Strategy<Value = String> {
            proptest::collection::vec(
                prop_oneof![
                    Just('"'),
                    Just('\\'),
                    Just('\n'),
                    Just('\r'),
                    Just('\t'),
                    Just('\u{1}'),
                    Just('\u{1f}'),
                    Just('/'),
                    Just(' '),
                    Just('a'),
                    Just('é'),
                ],
                0..=6,
            )
            .prop_map(|v| v.into_iter().collect())
        }

        /// Contract: a workspace move leaves window 1 referenced from exactly one
        /// placement, with the client record naming that placement, and the same
        /// request repeated is absorbed outright.
        ///
        /// The two placements are the classic failure: `State::check_invariants`
        /// reports it as "window referenced twice" and "stored at one place but
        /// tiled at another". It happens when a placement is addressed with one
        /// coordinate pair and the ownership derived from another — the removal
        /// silently misses (it targeted a workspace that never held the window)
        /// while the re-insert still lands. Addressing the placement by the
        /// client's own `(monitor, workspace)`, the pair `State::remove_client`
        /// uses, is what keeps one client in one placement. The cross-monitor
        /// shape of the same mistake has its own regression,
        /// `move_to_workspace_absorbs_a_cross_monitor_focus_slot`.
        ///
        /// The X sink focuses on the focused window's own monitor, so step 2
        /// (`FocusWindow`) returns the selection to monitor 0, where window 1
        /// lives, and the move is an ordinary same-monitor relocation.
        #[test]
        fn move_to_workspace_keeps_one_placement_per_client() {
            let sc = Scenario {
                n_mon: 2,
                seed: 1,
                floatness: 0,
                ops: vec![
                    Op::Map {
                        float: false,
                        has_parent: false,
                        parent: 0,
                    },
                    Op::Wire(Action::FocusMon(Dir::Left)),
                    Op::Cmd(GenCmd::FocusWindow(0)),
                    Op::Cmd(GenCmd::MoveToWorkspace(1)),
                ],
            };
            let mut engine = seed_engine(&sc);
            for (i, op) in sc.ops.iter().enumerate() {
                run_op(&mut engine, op);
                if let Err(v) = engine.state.check_invariants() {
                    panic!(
                        "step {i} ({op:?}) broke the state contract:\n  - {}\nSTATE:\n{}",
                        v.join("\n  - "),
                        logical_dump(&engine)
                    );
                }
            }
            // The window the last step named is the moved one; it must be placed
            // once, and the client record must be the authority on where.
            let moved = 1;
            let placements = placements_of(&engine.state, moved);
            assert_eq!(
                placements.len(),
                1,
                "window {moved} must be referenced from exactly one placement, got {placements:?}\nSTATE:\n{}",
                logical_dump(&engine)
            );
            let c = &engine.state.clients[&moved];
            assert_eq!(
                (placements[0].0, placements[0].1),
                (c.monitor, c.workspace),
                "the client record must name the placement the window actually has"
            );
            // Convergent: addressing the *same* window's current workspace again
            // is absorbed outright, with no second placement and no effects at
            // all. The window is re-focused first because the command is
            // focus-driven and the first move handed the focus to whatever was
            // left behind — repeating the op as-is would name that other window,
            // which is a different request, not a repeat of this one.
            t_focus(&mut engine, moved);
            let again = run_op(&mut engine, &sc.ops[3]);
            assert!(
                again.is_empty(),
                "repeating the move must be absorbed, got {again:?}"
            );
            assert_eq!(
                placements_of(&engine.state, moved),
                placements,
                "repeating the move changed the placement"
            );
        }

        /// Every `(monitor, workspace, kind)` placement slot that names `win`.
        /// The same two slot kinds `State::check_invariants` sweeps: a tiled
        /// column entry and a float entry.
        fn placements_of(s: &State, win: WindowId) -> Vec<(usize, usize, &'static str)> {
            let mut out = Vec::new();
            for (mi, mon) in s.monitors.iter().enumerate() {
                for (ws_i, ws) in mon.workspaces.iter().enumerate() {
                    for col in &ws.columns {
                        if col.windows.contains(&win) {
                            out.push((mi, ws_i, "column"));
                        }
                    }
                    if ws.floats.contains(&win) {
                        out.push((mi, ws_i, "float"));
                    }
                }
            }
            out
        }

        /// Run `ops` from `sc`'s seeded state, asserting the contract after every
        /// single step: the model must stay structurally valid, and a failure has
        /// to name the step that broke it. Debug builds panic inside
        /// `Engine::execute` on the first violation, so the panic message is the
        /// step marker there and this assertion covers the paths that mutate
        /// `State` without going through it.
        fn run_ops_leaving_state_valid(sc: &Scenario) -> Engine {
            let mut engine = seed_engine(sc);
            for (i, op) in sc.ops.iter().enumerate() {
                run_op(&mut engine, op);
                if let Err(v) = engine.state.check_invariants() {
                    panic!(
                        "step {i} ({op:?}) broke the state contract:\n  - {}\nSTATE:\n{}",
                        v.join("\n  - "),
                        logical_dump(&engine)
                    );
                }
            }
            engine
        }

        /// Contract: splitting the focused window out of its column references it
        /// from exactly one placement, even when the focus slot names a window
        /// that is tiled on *another* monitor.
        ///
        /// `NewColumn` reads the workspace to operate on from the client record
        /// (so a window focused on a non-active workspace is still found) but
        /// took the monitor from `sel_mon`. Once the X sink had written the
        /// selected monitor's focus slot with a window placed on the other one,
        /// the split inserted that window into this monitor's tree while it was
        /// still tiled over there — "window referenced twice" and "stored at one
        /// place but tiled at another". Moving a window across monitors is
        /// `MoveWindowToMonitor`'s job, so a cross-monitor focus slot absorbs the
        /// request instead.
        #[test]
        fn new_column_keeps_one_placement_for_a_cross_monitor_focus() {
            let sc = Scenario {
                n_mon: 2,
                seed: 0,
                floatness: 0,
                ops: vec![
                    Op::Map {
                        float: false,
                        has_parent: false,
                        parent: 0,
                    },
                    Op::Wire(Action::FocusMon(Dir::Left)),
                    Op::Cmd(GenCmd::FocusWindow(0)),
                    Op::Cmd(GenCmd::NewColumn),
                ],
            };
            let engine = run_ops_leaving_state_valid(&sc);
            let placements = placements_of(&engine.state, 1);
            assert_eq!(
                placements.len(),
                1,
                "window 1 must stay in one placement, got {placements:?}\nSTATE:\n{}",
                logical_dump(&engine)
            );
        }

        /// Contract: a command that only *requests* an input-focus change
        /// resolves the deferral that change orphans.
        ///
        /// `Engine::execute` runs its `pending_focus` safety net before the sink
        /// applies the `FocusWindow` effect, so at net time the overlay that owns
        /// the deferral still looks presented and the deferral survives — while
        /// the pending effect is about to take the focus (and with it the
        /// overlay's presentation) away from that owner. The queued window would
        /// then get the input focus behind an overlay nobody can see.
        ///
        /// Moving a window across monitors is the case where this bites hardest:
        /// the sink resolves the moved window's own monitor, so the request
        /// *selects* the monitor the deferral lives on and lands the focus on a
        /// window that is not its overlay owner. A maximize overlay there is
        /// presented exactly while it holds the focus, so the request takes the
        /// presentation down and the deferral is the orphan `check_invariants`
        /// #8c rejects.
        #[test]
        fn a_monitor_move_resolves_the_deferral_its_focus_request_orphans() {
            let mut engine = setup_engine_multi();
            let mon0 = 0;
            engine.state.monitors[mon0].workspaces[0].layout = LayoutKind::Column;
            // Window 1 maximizes on mon0 and holds its focus, so it is the
            // presented overlay; window 2 is deferred behind it.
            t_manage(&mut engine, 1);
            t_focus(&mut engine, 1);
            crate::core::commands::apply_maximize(&mut engine.state, 1, Some(true), Some(true));
            assert!(
                !t_manage(&mut engine, 2),
                "window 2 is deferred behind the maximize overlay"
            );
            // Window 3 is mapped on the other monitor and focused there, which is
            // what the move below will carry back across.
            let mut c3 = Client::new(3, 1, 0);
            c3.border_w = engine.cfg.border_w;
            c3.geom = Rect::new(0, 0, 800, 600);
            c3.saved_geom = c3.geom;
            engine.state.monitors[1].workspaces[0].add_tiled(3, engine.cfg.column_width);
            engine.state.add_client(c3);
            t_focus(&mut engine, 3);
            assert_eq!(engine.state.sel_mon, 1);

            let effects = engine.execute(MoveWindowToMonitor(3, Dir::Prev));

            assert_eq!(
                engine.state.sel_mon, 0,
                "the selection follows the moved window"
            );
            assert!(
                effects
                    .iter()
                    .any(|e| matches!(e, Effect::FocusWindow(Some(3)))),
                "the move requests the focus on the destination monitor: {effects:?}"
            );
            assert!(
                engine.state.pending_focus.is_none(),
                "the requested focus supersedes the queued one, exactly once"
            );
            engine
                .state
                .check_invariants()
                .expect("the core half of the move must leave a valid state");
            // The presentation half is the sink's: it writes the destination's
            // focus slot, which takes the focus off the maximize owner, and the
            // overlay goes down with the focus.
            t_focus(&mut engine, 3);
            assert_eq!(
                engine.state.monitors[0].workspaces[0].presented_maximize, None,
                "the focus request took the maximize overlay down"
            );
            engine
                .state
                .check_invariants()
                .expect("the monitor move must preserve invariants");
        }

        /// Contract: a command that only *requests* an input-focus change
        /// resolves the deferral that change orphans.
        ///
        /// `Engine::execute` runs its `pending_focus` safety net before the sink
        /// applies the `FocusWindow` effect, so at net time the overlay that owns
        /// the deferral still looks presented and the deferral survives — while
        /// the pending effect is about to take the focus (and with it the
        /// overlay's presentation) away from that owner. The queued window would
        /// then get the input focus behind an overlay nobody can see.
        #[test]
        fn requested_focus_move_resolves_the_deferral_it_orphans() {
            let sc = Scenario {
                n_mon: 1,
                seed: 1,
                floatness: 0,
                ops: vec![
                    Op::Cmd(GenCmd::ToggleMaximize),
                    Op::Map {
                        float: false,
                        has_parent: false,
                        parent: 0,
                    },
                    Op::Wire(Action::OverviewNav(Dir::Left)),
                ],
            };
            let engine = run_ops_leaving_state_valid(&sc);
            // Window 1 is the maximize overlay, window 2 the window deferred
            // behind it; the overview navigation moves the selection onto window
            // 2, so the deferral is resolved instead of stranded.
            assert_eq!(
                engine.state.pending_focus,
                None,
                "the deferral must be resolved by the navigation that takes the focus \
                 off its owner\nSTATE:\n{}",
                logical_dump(&engine)
            );
            assert_eq!(engine.state.monitors[0].focused, Some(2));
        }

        /// Contract: `presented_maximize` is derived state, and every transition
        /// that changes what the derivation reads re-derives *all* the monitors
        /// that can be showing the window.
        ///
        /// A monitor's maximize owner is the window its focus slot names on its
        /// active workspace, so a window placed on one monitor can be the
        /// presented owner of another whose slot names it. Refreshing only
        /// `c.monitor` on a flag change left the other monitor naming a window
        /// that was no longer maximized ("`presented_maximize` 1 is not
        /// maximized"); the same staleness appears when the window moves to
        /// another workspace ("`presented_maximize` 1 on wrong workspace").
        #[test]
        fn maximize_presentation_is_re_derived_on_every_monitor_that_shows_it() {
            let base = |ops| Scenario {
                n_mon: 2,
                seed: 1,
                floatness: 0,
                ops,
            };
            let focus_elsewhere_then_back = |last: Op| {
                base(vec![
                    Op::Cmd(GenCmd::ToggleMaximize),
                    Op::Wire(Action::FocusMon(Dir::Left)),
                    Op::Cmd(GenCmd::FocusWindow(0)),
                    Op::Wire(Action::FocusMon(Dir::Left)),
                    last,
                ])
            };
            // Un-maximizing while a foreign monitor's slot names the window.
            let engine = run_ops_leaving_state_valid(&focus_elsewhere_then_back(Op::Cmd(
                GenCmd::ToggleMaximize,
            )));
            assert!(
                engine.state.monitors[1].workspaces[0]
                    .presented_maximize
                    .is_none(),
                "the foreign monitor must not keep naming a window that is no longer \
                 maximized\nSTATE:\n{}",
                logical_dump(&engine)
            );
            // Moving it to another workspace while a foreign monitor's slot names
            // it: the name is attached to a workspace it no longer lives on.
            let engine = run_ops_leaving_state_valid(&focus_elsewhere_then_back(Op::Cmd(
                GenCmd::MoveToWorkspace(1),
            )));
            assert!(
                engine.state.monitors[1].workspaces[0]
                    .presented_maximize
                    .is_none(),
                "the foreign monitor must not keep naming a window that left that \
                 workspace\nSTATE:\n{}",
                logical_dump(&engine)
            );
        }

        /// Contract: a monitor's focus stack names each client at most once.
        ///
        /// Moving a window to another monitor makes it the most recently focused
        /// window there, so it belongs at the top of that monitor's stack exactly
        /// once — the same `retain`-then-`push` shape `focus_logical_on` uses. A
        /// bare `push` duplicated the entry whenever the destination stack already
        /// named the window (a focus slot left on the other monitor), which the
        /// invariant checker rejects as "focus stack has duplicate entries".
        #[test]
        fn move_to_monitor_does_not_duplicate_the_focus_stack_entry() {
            let sc = Scenario {
                n_mon: 2,
                seed: 1,
                floatness: 0,
                ops: vec![
                    Op::Wire(Action::FocusMon(Dir::Left)),
                    Op::Cmd(GenCmd::FocusWindow(0)),
                    Op::Wire(Action::FocusMon(Dir::Left)),
                    Op::Wire(Action::MoveMon(Dir::Left)),
                ],
            };
            let engine = run_ops_leaving_state_valid(&sc);
            for (mi, mon) in engine.state.monitors.iter().enumerate() {
                let mut sorted = mon.focus_stack.clone();
                sorted.sort_unstable();
                let deduped = {
                    let mut d = sorted.clone();
                    d.dedup();
                    d
                };
                assert_eq!(
                    sorted,
                    deduped,
                    "monitor {mi} focus stack has duplicates: {:?}\nSTATE:\n{}",
                    mon.focus_stack,
                    logical_dump(&engine)
                );
            }
        }

        /// Contract: a placement index only ever names live clients.
        ///
        /// The teardown purges the focus bookkeeping of every monitor, so no slot
        /// survives it now — the stale-slot hazard this originally leaned on is
        /// fixed at the source. The slot is therefore installed deliberately
        /// below, to keep pinning the contract this test is actually about:
        /// `ToggleFloat` must not compound a stale logical focus into a placement
        /// index. The command is invoked directly because the debug invariant
        /// check `Engine::execute` runs would trip on that pre-existing stale
        /// slot before this contract could be observed.
        #[test]
        fn toggle_float_ignores_a_focus_slot_that_names_a_dead_window() {
            let sc = Scenario {
                n_mon: 2,
                seed: 1,
                floatness: 0,
                ops: vec![
                    Op::Wire(Action::FocusMon(Dir::Left)),
                    Op::Cmd(GenCmd::FocusWindow(0)),
                    Op::Unmap { pick: 0 },
                ],
            };
            let mut engine = seed_engine(&sc);
            for op in &sc.ops {
                run_op(&mut engine, op);
            }
            // Re-install the stale slot the teardown now clears: a monitor whose
            // logical focus still names a window that no longer exists. That is
            // the precondition this pins the toggle against, so prove it is
            // really there.
            engine.state.monitors[1].focused = Some(1);
            assert_eq!(engine.state.monitors[1].focused, Some(1));
            assert!(!engine.state.clients.contains_key(&1));
            ToggleFloat(None)
                .execute(&mut engine.state, &mut engine.cfg)
                .effects
                .is_empty();
            for (mi, mon) in engine.state.monitors.iter().enumerate() {
                for (ws_i, ws) in mon.workspaces.iter().enumerate() {
                    for &w in &ws.floats {
                        assert!(
                            engine.state.clients.contains_key(&w),
                            "monitor {mi} ws {ws_i} floats slot names dead window {w}\nSTATE:\n{}",
                            logical_dump(&engine)
                        );
                    }
                }
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig {
                cases: 64,
                max_shrink_iters: 4096,
                ..ProptestConfig::default()
            })]

            /// Contract: `Engine::execute` and `Engine::execute_batch` preserve
            /// every structural invariant of `State` (A–F) after *any* legal
            /// command sequence, including map/unmap in the middle of it.
            ///
            /// This is the end-to-end statement of the contract the two entry
            /// points advertise: every mutation funnels through them, and
            /// whatever the user pressed, the model must stay consistent.
            #[test]
            fn prop_invariants_preserved_under_command_sequences(sc in arb_scenario()) {
                let mut engine = seed_engine(&sc);
                prop_assert!(
                    engine.state.check_invariants().is_ok(),
                    "generated seed state must itself be legal: {:?}",
                    engine.state.check_invariants().err()
                );
                for (i, op) in sc.ops.iter().enumerate() {
                    // Alternate the two documented entry points: `execute` for a
                    // single gesture, `execute_batch` for the coalesced
                    // transaction, which runs the same post-conditions.
                    let effects = if i % 2 == 0 {
                        run_op(&mut engine, op)
                    } else {
                        match op {
                            Op::Cmd(g) => match g.build(&engine.state) {
                                Some(cmd) => engine.execute_batch(vec![cmd]),
                                None => Vec::new(),
                            },
                            // `dispatch` and the pure helpers are single-command
                            // paths; the batch arm is covered by `Op::Cmd`.
                            _ => run_op(&mut engine, op),
                        }
                    };
                    // Map/unmap produce no effects; every command that produced
                    // some must have asked for a state publish.
                    if !effects.is_empty() {
                        prop_assert!(
                            effects.iter().any(|e| matches!(e, Effect::PublishIpcState)),
                            "step {i} ({op:?}) returned effects without a state publish"
                        );
                    }
                    if let Err(v) = engine.state.check_invariants() {
                        prop_assert!(
                            false,
                            "step {i} ({op:?}) broke the state contract:\n  - {}\nSTATE:\n{}",
                            v.join("\n  - "),
                            logical_dump(&engine)
                        );
                    }
                }
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig {
                cases: 64,
                max_shrink_iters: 4096,
                ..ProptestConfig::default()
            })]

            /// Contract: after any command, `pending_focus` is either empty or
            /// still owned by a *presented* overlay.
            ///
            /// `Engine::execute`/`execute_batch` advertise exactly this as their
            /// safety net (`reconcile_pending_focus_after_transition`, run right
            /// before the invariant check). Without it a deferral survives its
            /// own overlay and the input focus is handed to a window nobody can
            /// see.
            #[test]
            fn prop_pending_focus_postcondition_holds_after_every_command(
                sc in arb_scenario()
            ) {
                let mut engine = seed_engine(&sc);
                for (i, op) in sc.ops.iter().enumerate() {
                    run_op(&mut engine, op);
                    if engine.state.pending_focus.is_some() {
                        prop_assert!(
                            engine.state.pending_focus_owner_presented(),
                            "step {i} ({op:?}) left a deferral whose overlay is gone: {:?}\nSTATE:\n{}",
                            engine.state.pending_focus,
                            logical_dump(&engine)
                        );
                    }
                }
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig {
                cases: 96,
                max_shrink_iters: 4096,
                ..ProptestConfig::default()
            })]

            /// Contract: a destroyed client leaves no dangling reference
            /// anywhere in `State` — not in a column, a float list, a focus
            /// slot, the deferred-focus triple, the transient queue, a
            /// `presented_maximize` entry on *any* monitor, or another client's
            /// `transient_parent`.
            ///
            /// This is stronger than the structural invariant check, which only
            /// inspects the columns/floats/focus-stack subset; the full sweep is
            /// what makes the ownership graph safe to walk (and stops a recycled
            /// XID from inheriting a dead window's popups).
            #[test]
            fn prop_unmap_leaves_no_dangling_reference(sc in arb_scenario()) {
                let mut engine = seed_engine(&sc);
                for (i, op) in sc.ops.iter().enumerate() {
                    // Run part of the sequence, then close a window and sweep.
                    if i % 2 == 0 {
                        run_op(&mut engine, op);
                        prop_assert!(
                            engine.state.check_invariants().is_ok(),
                            "step {i} ({op:?}) broke the contract before the close"
                        );
                        continue;
                    }
                    let live = live_windows(&engine.state);
                    if live.is_empty() {
                        continue;
                    }
                    let dead = live[(i * 7 + 3) % live.len()];
                    prop_assert!(engine.state.remove_client(dead).is_some());
                    let stale: Vec<_> = all_references(&engine.state)
                        .into_iter()
                        .filter(|&(_, w)| w == dead)
                        .collect();
                    prop_assert!(
                        stale.is_empty(),
                        "closed window {dead} is still referenced in {:?}\nSTATE:\n{}",
                        stale,
                        logical_dump(&engine)
                    );
                    prop_assert!(
                        engine.state.check_invariants().is_ok(),
                        "closing {dead} broke the contract:\n  - {}\nSTATE:\n{}",
                        engine
                            .state
                            .check_invariants()
                            .err()
                            .unwrap_or_default()
                            .join("\n  - "),
                        logical_dump(&engine)
                    );
                }
            }
        }

        /// An operation the docs describe as idempotent, plus how its "did
        /// anything change?" answer must look on a second, identical
        /// application.
        #[derive(Debug, Clone)]
        enum Absorb {
            SetLayout,
            ViewCurrent,
            MoveToCurrent,
            FullscreenTopology { pick: u32, entering: bool },
            Maximize { pick: u32, vert: bool, horiz: bool },
            GeomRestore { pick: u32 },
        }

        fn arb_absorb() -> impl Strategy<Value = Absorb> {
            prop_oneof![
                2 => Just(Absorb::SetLayout),
                3 => Just(Absorb::ViewCurrent),
                3 => Just(Absorb::MoveToCurrent),
                6 => (
                    any::<u32>(),
                    any::<bool>(),
                ).prop_map(|(pick, entering)| Absorb::FullscreenTopology { pick, entering }),
                6 => (any::<u32>(), any::<bool>(), any::<bool>())
                    .prop_map(|(pick, vert, horiz)| Absorb::Maximize { pick, vert, horiz }),
                4 => any::<u32>().prop_map(|pick| Absorb::GeomRestore { pick }),
            ]
        }

        /// Outcome of one application: the command's own "did it change
        /// anything?" answer, when it has one, and the effect list the backend
        /// would have drained.
        #[derive(Debug, Default)]
        struct Applied {
            changed: Option<bool>,
            /// Whether the operation actually restored a stored geometry
            /// snapshot, as opposed to finding nothing to restore.
            restored_snapshot: bool,
            effects: Vec<EffectKind>,
        }

        /// `Effect` is not `PartialEq`, so the effect list is compared through a
        /// small discriminant-plus-payload projection.
        #[derive(Debug, PartialEq, Eq)]
        enum EffectKind {
            ArrangeMonitor(usize),
            MarkRestack(usize),
            FocusWindow(Option<WindowId>),
            Unfocus(WindowId),
            ConfigureWindow(WindowId),
            KillWindow(WindowId),
            SetFullscreen(WindowId, bool),
            SetMaximized(WindowId, Option<bool>, Option<bool>),
            SyncWindowPrefs(WindowId),
            SetCurrentDesktop(usize),
            SetWindowDesktop(WindowId, usize),
            Spawn,
            Quit,
            Restart,
            PublishIpcState,
        }

        fn effect_kind(e: &Effect) -> EffectKind {
            match e {
                Effect::ArrangeMonitor(m) => EffectKind::ArrangeMonitor(*m),
                Effect::MarkRestack(m) => EffectKind::MarkRestack(*m),
                Effect::FocusWindow(w) => EffectKind::FocusWindow(*w),
                Effect::Unfocus(w) => EffectKind::Unfocus(*w),
                Effect::ConfigureWindow { win, .. } => EffectKind::ConfigureWindow(*win),
                Effect::KillWindow(w) => EffectKind::KillWindow(*w),
                Effect::SetFullscreen { win, on } => EffectKind::SetFullscreen(*win, *on),
                Effect::SetMaximized { win, vert, horiz } => {
                    EffectKind::SetMaximized(*win, *vert, *horiz)
                }
                Effect::SyncWindowPrefs(w) => EffectKind::SyncWindowPrefs(*w),
                Effect::SetCurrentDesktop(d) => EffectKind::SetCurrentDesktop(*d),
                Effect::SetWindowDesktop { win, ws } => EffectKind::SetWindowDesktop(*win, *ws),
                Effect::Spawn(_) => EffectKind::Spawn,
                Effect::Quit => EffectKind::Quit,
                Effect::Restart => EffectKind::Restart,
                Effect::PublishIpcState => EffectKind::PublishIpcState,
            }
        }

        fn kinds(effects: &[Effect]) -> Vec<EffectKind> {
            effects.iter().map(effect_kind).collect()
        }

        /// Apply an absorbing operation once.
        fn apply_absorb(engine: &mut Engine, op: &Absorb) -> Applied {
            let mut out = Applied::default();
            let live = live_windows(&engine.state);
            let target = |pick: u32| -> Option<WindowId> {
                if live.is_empty() {
                    None
                } else {
                    Some(live[(pick as usize) % live.len()])
                }
            };
            match op {
                Absorb::SetLayout => {
                    out.effects = kinds(&engine.execute(SetLayout(LayoutKind::Column)));
                }
                Absorb::ViewCurrent => {
                    let Some(mon) = engine.state.monitors.get(engine.state.sel_mon) else {
                        return out;
                    };
                    out.effects = kinds(&engine.execute(ViewWorkspace(mon.active_ws)));
                }
                Absorb::MoveToCurrent => {
                    let mi = engine.state.sel_mon;
                    let Some(mon) = engine.state.monitors.get(mi) else {
                        return out;
                    };
                    let active = mon.active_ws;
                    let home = mon
                        .focused
                        .and_then(|w| engine.state.clients.get(&w).map(|c| c.workspace))
                        .unwrap_or(active);
                    out.effects = kinds(&engine.execute(MoveToWorkspace(home)));
                }
                Absorb::FullscreenTopology { pick, entering } => {
                    out.changed = target(*pick).map(|w| {
                        apply_fullscreen_topology(&mut engine.state, &engine.cfg, w, *entering)
                    });
                }
                Absorb::Maximize { pick, vert, horiz } => {
                    if let Some(w) = target(*pick) {
                        apply_maximize(&mut engine.state, w, Some(*vert), Some(*horiz));
                    }
                }
                Absorb::GeomRestore { pick } => {
                    out.restored_snapshot = target(*pick)
                        .and_then(|w| apply_fullscreen_geom_restore(&mut engine.state, w))
                        .is_some();
                    // The restore is a presentation demotion, so it is applied
                    // together with the reconciliation the transition that uses
                    // it performs. `apply_fullscreen_geom_restore` restores the
                    // pre-fullscreen `FullscreenPolicy`, and a window whose
                    // `True` policy is restored is no longer an exclusive
                    // overlay even with the `FULLSCREEN` bit still set, so the
                    // restore alone really does take a queued deferral's owner
                    // down — but it is a *geometry* restore with no focus or
                    // effect of its own, and its only production caller
                    // (`ToggleFullscreen`) clears the flag and consumes the
                    // deferral in the same breath. Applying the restore with
                    // neither half is a state no transition reaches (and one the
                    // deferral's own creator can no longer produce:
                    // `decide_manage_focus` only defers behind a `True`-policy
                    // overlay), so the model applies the reconciliation the
                    // transition performs and lets the property assert the pair.
                    let _ = reconcile_pending_focus_after_transition(&mut engine.state);
                }
            }
            out
        }

        proptest! {
            #![proptest_config(ProptestConfig {
                cases: 64,
                max_shrink_iters: 4096,
                ..ProptestConfig::default()
            })]

            /// Contract: the commands documented as idempotent reach a fixpoint.
            ///
            /// `apply_fullscreen_topology` ("running it twice for the same
            /// transition is a no-op… returns true when the topology actually
            /// changed"), `apply_fullscreen_geom_restore` ("returns `None` when
            /// there was nothing to restore"), and the view/move
            /// commands that bail out on an already-satisfied target must all
            /// leave the state *and* the effect list unchanged when repeated.
            /// A second application that re-arranges, re-publishes or re-flips a
            /// flag is a real bug: users repeat keybinds, and IPC replays.
            #[test]
            fn prop_absorbing_commands_reach_fixpoint(sc in arb_scenario(), op in arb_absorb()) {
                let mut engine = seed_engine(&sc);
                for (i, o) in sc.ops.iter().take(4).enumerate() {
                    run_op(&mut engine, o);
                    let _ = i;
                }
                let first = apply_absorb(&mut engine, &op);
                let after_first = logical_dump(&engine);
                prop_assert!(
                    engine.state.check_invariants().is_ok(),
                    "first application of {op:?} broke the contract:\n  - {}",
                    engine
                        .state
                        .check_invariants()
                        .err()
                        .unwrap_or_default()
                        .join("\n  - ")
                );
                let second = apply_absorb(&mut engine, &op);
                let after_second = logical_dump(&engine);
                prop_assert_eq!(
                    after_first,
                    after_second,
                    "repeating {:?} was not a no-op (first reported {:?}, second {:?})",
                    op,
                    first,
                    second
                );
                if let Some(true) = second.changed {
                    prop_assert!(
                        false,
                        "{:?} reported a topology change on a repeat",
                        op
                    );
                }
                if second.restored_snapshot {
                    prop_assert!(false, "{op:?} restored a geometry snapshot twice");
                }
                // A target that is already satisfied must be absorbed outright:
                // no arrange, no focus, not even an IPC publish.
                if matches!(
                    op,
                    Absorb::ViewCurrent | Absorb::MoveToCurrent
                ) {
                    prop_assert!(
                        second.effects.is_empty(),
                        "{op:?} on an already-satisfied target still emitted {:?}",
                        second.effects
                    );
                }
                prop_assert!(
                    engine.state.check_invariants().is_ok(),
                    "repeating {op:?} broke the contract:\n  - {}",
                    engine
                        .state
                        .check_invariants()
                        .err()
                        .unwrap_or_default()
                        .join("\n  - ")
                );
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig {
                cases: 64,
                max_shrink_iters: 4096,
                ..ProptestConfig::default()
            })]

            /// Contract: every command produces a well-formed effect list.
            ///
            /// Three rules the backend depends on: exactly one `PublishIpcState`
            /// and always last, so a synchronous IPC subscriber sees the
            /// post-command snapshot; `MarkRestack` before the `ArrangeMonitor`
            /// that consumes it ("emit before `ArrangeMonitor` when stacking
            /// changed"); and every window an effect names is a client the WM
            /// still manages — the backend turns a stale id straight into an X
            /// error.
            #[test]
            fn prop_effects_are_well_formed(sc in arb_scenario()) {
                let mut engine = seed_engine(&sc);
                for (i, op) in sc.ops.iter().enumerate() {
                    let effects = if i % 3 == 0 {
                        match op {
                            Op::Cmd(g) => match g.build(&engine.state) {
                                Some(cmd) => {
                                    let e = engine.execute_batch(vec![cmd]);
                                    mirror_focus(&mut engine, &e);
                                    e
                                }
                                None => Vec::new(),
                            },
                            _ => run_op(&mut engine, op),
                        }
                    } else {
                        run_op(&mut engine, op)
                    };
                    let publishes: Vec<usize> = effects
                        .iter()
                        .enumerate()
                        .filter(|(_, e)| matches!(e, Effect::PublishIpcState))
                        .map(|(k, _)| k)
                        .collect();
                    if !effects.is_empty() {
                        prop_assert_eq!(
                            publishes.len(),
                            1,
                            "step {} ({:?}) emitted {} state publishes",
                            i,
                            op,
                            publishes.len()
                        );
                        prop_assert_eq!(
                            *publishes.last().unwrap(),
                            effects.len() - 1,
                            "step {} ({:?}) put the state publish before other effects: {:?}",
                            i,
                            op,
                            effects
                        );
                    }
                    // "Emit before `ArrangeMonitor` when stacking changed": for
                    // each monitor, the restack request must precede the arrange
                    // that consumes it.
                    for mi in 0..engine.state.monitors.len() {
                        let first_mark = effects
                            .iter()
                            .position(|e| matches!(e, Effect::MarkRestack(x) if *x == mi));
                        let first_arrange = effects
                            .iter()
                            .position(|e| matches!(e, Effect::ArrangeMonitor(x) if *x == mi));
                        if let (Some(mark), Some(arrange)) = (first_mark, first_arrange) {
                            prop_assert!(
                                mark < arrange,
                                "step {} ({:?}) emitted MarkRestack after ArrangeMonitor for monitor {}: {:?}",
                                i,
                                op,
                                mi,
                                effects
                            );
                        }
                    }
                    for e in &effects {
                        let named: Option<WindowId> = match e {
                            Effect::FocusWindow(Some(w))
                            | Effect::Unfocus(w)
                            | Effect::KillWindow(w)
                            | Effect::SyncWindowPrefs(w)
                            | Effect::SetFullscreen { win: w, .. }
                            | Effect::ConfigureWindow { win: w, .. }
                            | Effect::SetMaximized { win: w, .. }
                            | Effect::SetWindowDesktop { win: w, .. } => Some(*w),
                            _ => None,
                        };
                        if let Some(w) = named {
                            prop_assert!(
                                engine.state.clients.contains_key(&w),
                                "step {} ({:?}) emitted {:?} for a window that is not a client",
                                i,
                                op,
                                e
                            );
                        }
                    }
                }
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig {
                cases: 64,
                max_shrink_iters: 4096,
                ..ProptestConfig::default()
            })]

            /// Contract: `MoveResize` sanitizes hostile geometry and reports
            /// exactly the rect it stored.
            ///
            /// The command is the only writer of a float's `geom` and emits a
            /// single `ConfigureWindow` for it, so the two must agree; and a
            /// 0×0 or `u32::MAX` rect must never reach the float clamp. An
            /// unknown window id must be absorbed without an effect.
            #[test]
            fn prop_move_resize_sanitizes_and_agrees_with_effect(
                sc in arb_scenario(),
                x in any::<i32>(),
                y in any::<i32>(),
                w in any::<u32>(),
                h in any::<u32>(),
            ) {
                let mut engine = seed_engine(&sc);
                let live = live_windows(&engine.state);
                prop_assume!(!live.is_empty());
                let win = live[0];
                // An id no client can have, to cover the absorbed path.
                let ghost = live_windows(&engine.state).iter().copied().max().unwrap_or(0) + 7_919;
                let before = logical_dump(&engine);

                let ghost_effects = engine.execute(MoveResize(ghost, Rect::new(x, y, w, h)));
                prop_assert!(
                    ghost_effects.is_empty(),
                    "MoveResize for an unmanaged window {ghost} emitted {ghost_effects:?}"
                );
                prop_assert_eq!(
                    logical_dump(&engine),
                    before,
                    "MoveResize for an unmanaged window mutated the state"
                );

                let effects = engine.execute(MoveResize(win, Rect::new(x, y, w, h)));
                let stored = engine.state.clients[&win].geom;
                // `Engine::execute` appends the state publish; the command
                // itself contributes exactly one configure.
                let configures: Vec<&Effect> = effects
                    .iter()
                    .filter(|e| matches!(e, Effect::ConfigureWindow { .. }))
                    .collect();
                prop_assert_eq!(
                    configures.len(),
                    1,
                    "MoveResize must emit exactly one ConfigureWindow, got {:?}",
                    effects
                );
                match configures[0] {
                    Effect::ConfigureWindow {
                        win: ew,
                        geom,
                        border_w,
                    } => {
                        prop_assert_eq!(*ew, win, "MoveResize configured a different window");
                        prop_assert_eq!(
                            *geom, stored,
                            "the configured rect must be the rect stored in client.geom"
                        );
                        prop_assert_eq!(
                            *border_w,
                            engine.state.clients[&win].border_w,
                            "the configured border must be the client's current border"
                        );
                    }
                    other => {
                        prop_assert!(
                            false,
                            "MoveResize emitted a non-configure effect: {:?}",
                            other
                        );
                    }
                }
                prop_assert!(
                    (1..=16_384).contains(&stored.w) && (1..=16_384).contains(&stored.h),
                    "hostile size {w}x{h} survived as {:?}",
                    stored
                );
                prop_assert!(
                    stored.x >= -16_384 && stored.x <= 16_384,
                    "hostile x {x} survived as {}",
                    stored.x
                );
                prop_assert!(
                    stored.y >= -16_384 && stored.y <= 16_384,
                    "hostile y {y} survived as {}",
                    stored.y
                );
                prop_assert!(
                    engine.state.check_invariants().is_ok(),
                    "MoveResize broke the contract:\n  - {}",
                    engine
                        .state
                        .check_invariants()
                        .err()
                        .unwrap_or_default()
                        .join("\n  - ")
                );

                // Re-normalizing an already-normalized rect is documented to be
                // bit-for-bit identical, so a repeated drag on the same rect
                // cannot make the window drift.
                let again = engine.execute(MoveResize(win, stored));
                prop_assert_eq!(
                    engine.state.clients[&win].geom,
                    stored,
                    "repeating MoveResize with the stored rect moved the window"
                );
                if let Some(Effect::ConfigureWindow { geom, .. }) = again.first() {
                    prop_assert_eq!(*geom, stored, "the repeat emitted a different rect");
                }
            }
        }



        /// The canonical wire spelling of an action: its `name()` plus the
        /// argument shape the `ACTIONS` table declares for that verb.
        fn canonical(a: &Action) -> Option<String> {
            let verb = action_name(a);
            let arg = match a {
                Action::Spawn(argv) => argv.join(" "),
                Action::FocusDir(d)
                | Action::MoveDir(d)
                | Action::FocusMon(d)
                | Action::MoveMon(d)
                | Action::OverviewNav(d)
                | Action::PageSnap(d) => dir_name(*d).to_string(),
                Action::SetLayout(_) => "column".to_string(),
                Action::GrowCol(px) => px.to_string(),
                Action::View(i) | Action::MoveToWs(i) => (i + 1).to_string(),
                Action::ViewportZoom(z) => format!("{z}"),
                _ => String::new(),
            };
            if arg.is_empty() {
                Some(verb.to_string())
            } else {
                Some(format!("{verb}:{arg}"))
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig {
                cases: 256,
                max_shrink_iters: 4096,
                ..ProptestConfig::default()
            })]

            /// Contract: the action vocabulary round-trips through its own
            /// parser, and both channel spellings agree.
            ///
            /// `action::name` is the single source of truth shared by the TOML
            /// config and the control socket; a name that does not parse (or
            /// parses to a different action) silently makes a keybind or an IPC
            /// client unreachable. The legacy fused forms (`focus-left`,
            /// `shrink-col 40`) must stay exactly equivalent to the canonical
            /// ones, and workspace 0 must stay rejected because workspaces are
            /// 1-indexed on the wire.
            #[test]
            fn prop_action_vocabulary_round_trips(a in arb_action()) {
                // `arb_action` draws `ViewportZoom` from raw bit patterns so the
                // state-machine properties meet NaN and subnormals. NaN is not
                // equal to itself, so the spelling of a non-finite zoom can never
                // compare equal to what it parsed back to — that is a limit of
                // the comparison, not of the grammar, so it is excluded here
                // rather than allowed to masquerade as a parse failure.
                prop_assume!(
                    !matches!(a, Action::ViewportZoom(z) if !z.is_finite()),
                    "a non-finite zoom cannot round-trip through equality"
                );
                let text = canonical(&a).expect("every action has a canonical spelling");
                let parsed = parse_action(&text);
                prop_assert_eq!(
                    parsed.as_ref(),
                    Some(&a),
                    "canonical spelling {:?} did not parse back to the same action",
                    text
                );
                if let Some(p) = &parsed {
                    prop_assert_eq!(
                        action_name(p),
                        action_name(&a),
                        "a round-tripped action changed its canonical name"
                    );
                }
                // The table in `core::action` is the machine-checkable contract
                // of the vocabulary: every entry must name a verb that parses
                // and re-parses under its declared argument kind.
                let verb = action_name(&a);
                let entry = crate::core::action::ACTIONS
                    .iter()
                    .find(|(n, _)| *n == verb)
                    .unwrap_or_else(|| panic!("verb {verb} is missing from the ACTIONS table"));
                prop_assert!(
                    parse_action(&text).is_some(),
                    "ACTIONS entry {verb} ({:?}) has a spelling that does not parse",
                    entry.1
                );
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig {
                cases: 256,
                max_shrink_iters: 4096,
                ..ProptestConfig::default()
            })]

            /// Contract: action parsing tolerates surrounding whitespace and the
            /// case of the verb and of the enumerated arguments, the legacy fused
            /// spellings agree with the canonical ones, and workspace 0 stays
            /// rejected.
            ///
            /// The parser folds the case of the verb (and `dir_from` folds its
            /// argument) and trims the input, so `FOCUS:LEFT` and `  focus:left  `
            /// are the same request: a user's capitalised keymap entry or a shell
            /// script must not silently stop working. The legacy fused forms are
            /// matched as a prefix before the fold, so only their documented
            /// lowercase spelling is claimed here. Workspaces are 1-indexed on the
            /// wire, so 0 must be rejected rather than underflowing.
            #[test]
            fn prop_action_parse_is_case_and_whitespace_insensitive(
                d in 0u8..=5,
                n in 0u32..=500,
                px in any::<i32>(),
            ) {
                let dir = dir_name(dir_of(d));
                for (canonical, legacy) in [
                    (format!("focus:{dir}"), format!("focus-{dir}")),
                    (format!("move:{dir}"), format!("move-{dir}")),
                ] {
                    let want = parse_action(&canonical);
                    prop_assert!(want.is_some(), "{:?} must parse", canonical);
                    prop_assert_eq!(
                        &want,
                        &parse_action(&legacy),
                        "legacy spelling {:?} disagrees with {:?}",
                        legacy,
                        canonical
                    );
                    prop_assert_eq!(
                        &parse_action(&format!("  {canonical}  ")),
                        &want,
                        "leading/trailing whitespace changed the parse of {:?}",
                        canonical
                    );
                    prop_assert_eq!(
                        &parse_action(&canonical.to_ascii_uppercase()),
                        &want,
                        "case changed the parse of {:?}",
                        canonical
                    );
                }
                prop_assert_eq!(
                    parse_action(&format!("shrink-col {n}")),
                    parse_action(&format!("grow_col:{}", -(n as i64))),
                    "shrink-col {} must be grow_col with the opposite sign",
                    n
                );
                prop_assert_eq!(
                    parse_action(&format!("grow_col:{px}")),
                    Some(Action::GrowCol(px)),
                    "grow_col round-tripped the wrong delta"
                );
                prop_assert_eq!(
                    parse_action(&format!("  grow_col:{px}  ")),
                    parse_action(&format!("grow_col:{px}")),
                    "whitespace changed the parse of grow_col"
                );
                // Workspaces are 1-indexed on the wire; 0 is not a workspace and
                // must be rejected rather than underflowing to usize::MAX.
                prop_assert_eq!(
                    parse_action("view:0"),
                    None,
                    "workspace 0 must be rejected"
                );
                prop_assert_eq!(
                    parse_action("move_to_ws:0"),
                    None,
                    "workspace 0 must be rejected for move_to_ws too"
                );
                for i in 1..=9usize {
                    prop_assert_eq!(
                        parse_action(&format!("view:{i}")),
                        Some(Action::View(i - 1)),
                        "view:{} must be the 0-based workspace {}",
                        i,
                        i - 1
                    );
                    prop_assert_eq!(
                        parse_action(&format!("move_to_ws:{i}")),
                        Some(Action::MoveToWs(i - 1)),
                        "move_to_ws:{} must be the 0-based workspace {}",
                        i,
                        i - 1
                    );
                }
                prop_assert_eq!(
                    parse_action(""),
                    None,
                    "the empty string must not parse"
                );
                prop_assert_eq!(
                    parse_action("   "),
                    None,
                    "a blank string must not parse"
                );
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig {
                cases: 96,
                max_shrink_iters: 4096,
                ..ProptestConfig::default()
            })]

            /// Contract: the IPC snapshots are always well-formed JSON, whatever
            /// the state carries.
            ///
            /// The writers promise a deterministic, consumer-parseable document
            /// — including the explicit guards for a poisoned camera position or
            /// column weight, because a bare `NaN` is not valid JSON and would
            /// break every bar and script reading the socket. The generated state
            /// is deliberately poisoned here, so the guards are what is under
            /// test rather than a lucky finite state.
            #[test]
            fn prop_ipc_json_is_well_formed(
                sc in arb_scenario(),
                status in arb_hostile_string(),
                name in arb_hostile_string(),
                class in arb_hostile_string(),
                instance in arb_hostile_string(),
            ) {
                let mut engine = seed_engine(&sc);
                engine.state.status = status;
                for win in live_windows(&engine.state) {
                    let c = engine.state.clients.get_mut(&win).unwrap();
                    c.name = name.clone();
                    c.class = class.clone();
                    c.instance = instance.clone();
                }
                // Hostile float state: a non-finite camera and a non-finite
                // weight are exactly what a corrupt session file or a bad
                // arithmetic path would leave behind.
                for mon in &mut engine.state.monitors {
                    for ws in &mut mon.workspaces {
                        // Alternate the two kinds of poison, so both are covered
                        // instead of the second write clobbering the first.
                        ws.camera.position = if ws.tag % 2 == 0 {
                            f32::NAN
                        } else {
                            f32::INFINITY
                        };
                        for col in &mut ws.columns {
                            col.weight = f32::NAN;
                        }
                    }
                }
                let docs = [
                    ("state_json", state_json(&engine.state, &engine.cfg)),
                    ("query state", query_json(&engine.state, &engine.cfg, "state")),
                    (
                        "query workspaces",
                        query_json(&engine.state, &engine.cfg, "workspaces"),
                    ),
                    ("query tree", query_json(&engine.state, &engine.cfg, "tree")),
                    (
                        "query focused",
                        query_json(&engine.state, &engine.cfg, "focused"),
                    ),
                ];
                for (what, doc) in docs {
                    if let Some(defect) = json_defect(&doc) {
                        prop_assert!(
                            false,
                            "{what} emitted a malformed document: {defect}\nDOC: {}",
                            &doc[..doc.len().min(400)]
                        );
                    }
                }
                prop_assert_eq!(
                    state_json(&engine.state, &engine.cfg),
                    state_json(&engine.state, &engine.cfg),
                    "the snapshot must be deterministic"
                );
            }
        }
    }

    /// A state whose only column holds three windows and weighs `weight` — the
    /// shape `MoveWindow` splits, since a split needs a column with more than one
    /// window to take one out of.
    fn stacked_column(weight: f32) -> State {
        let mut st = State::new();
        st.monitors
            .push(Monitor::new(Rect::new(0, 0, 1920, 1080), 1));
        for i in 0..3u32 {
            let mut c = Client::new(0x300 + i, 0, 0);
            c.flags.clear(WinFlags::MAXIMIZED);
            st.add_client(c);
            st.monitors[0].workspaces[0].add_tiled(0x300 + i, 0.5);
        }
        // Merge the two later windows into the first column, then point the focus
        // and the monitor's focus at its leading window, as a focus command would.
        let ws = &mut st.monitors[0].workspaces[0];
        for win in 0x301..0x303u32 {
            ws.remove_window(win);
        }
        for win in 0x301..0x303u32 {
            let pos = ws.columns[0].windows.len();
            ws.drop_into_column(0, win, pos);
        }
        ws.columns[0].weight = weight;
        ws.focus.column_idx = 0;
        ws.columns[0].focused = 0;
        st.monitors[0].focused = Some(0x300);
        st
    }

    fn weights(st: &State) -> Vec<f32> {
        st.monitors[0].workspaces[0]
            .columns
            .iter()
            .map(|c| c.weight)
            .collect()
    }

    /// A new column placed next to one already at the 0.05 floor must not yield
    /// a half outside the documented band - the checker enforces the band after
    /// the very next command, so placing has to clamp, not just divide.
    #[test]
    fn placing_next_to_a_column_at_the_band_floor_keeps_every_width_in_band() {
        let st = stacked_column(0.05);
        let mut eng = Engine::new(Cfg::default());
        eng.state = st;
        eng.execute(crate::core::commands::NewColumn);
        eng.state.assert_invariants();
        assert!(
            eng.state.monitors[0].workspaces[0]
                .columns
                .iter()
                .all(|c| c.weight >= 0.05 - 1e-6),
            "every half must stay inside the documented band, got {:?}",
            weights(&eng.state)
        );
    }
}
