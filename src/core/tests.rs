#[cfg(test)]
mod unit_tests {
    use crate::config::Cfg;
    use crate::core::desired::DesiredState;
    use crate::core::layout::{FsCtx, LayoutRegistry, RibbonScratch};
    use crate::core::Engine;
    use crate::types::{
        Action, Client, FullscreenPolicy, LayoutKind, Monitor, Rect, WinFlags, WindowId,
    };

    fn default_registry() -> LayoutRegistry {
        LayoutRegistry::new()
    }
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

    // 2. Helper to initialize Engine with a default monitor,
    // simulating a real desktop environment ready to receive windows.
    fn setup_engine() -> Engine {
        let mut engine = Engine::new(default_cfg());
        engine
            .state
            .monitors
            .push(Monitor::new(Rect::new(0, 0, 1920, 1080), 9));
        engine
    }

    /// Two side-by-side monitors, each with 9 workspaces, so a test can place a
    /// real overlay + `pending_focus` on a DIFFERENT monitor/workspace than the
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
    fn test_cycle_layout_wraps_around() {
        let mut engine = setup_engine();

        assert_eq!(
            engine.state.monitors[0].workspaces[0].layout,
            LayoutKind::Column
        );

        engine.dispatch(Action::CycleLayout);
        assert_eq!(
            engine.state.monitors[0].workspaces[0].layout,
            LayoutKind::Grid
        );

        engine.dispatch(Action::CycleLayout);
        assert_eq!(
            engine.state.monitors[0].workspaces[0].layout,
            LayoutKind::Column,
            "layout cycle must wrap Column→Grid→Column",
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
        let registry = default_registry();
        arrange(
            &engine.state,
            mi,
            &engine.cfg,
            &registry,
            crate::core::layout::Phase::Live,
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
        // Directly exercise the shared pure helper both the backend and the
        // engine now delegate to (single source of truth).
        use crate::types::Workspace;
        let mut ws = Workspace::new(0);
        assert_eq!(ws.layout, LayoutKind::Column);
        assert_eq!(ws.cycle_layout(), LayoutKind::Grid);
        assert_eq!(ws.cycle_layout(), LayoutKind::Column);
    }

    // ── move_dir tests ──────────────────────────────────────────────────────

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
            boost: 1.0,
        });
        ws.columns.push(Column {
            windows: vec![20],
            focused: 0,
            weight: 0.5,
            boost: 1.0,
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
            boost: 1.0,
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
            boost: 1.0,
        });
        ws.focus = Focus { column_idx: 0 };
        engine.state.monitors[0].focused = Some(10);

        let changed = engine.state.apply_move_dir(crate::types::Dir::Right);
        assert!(!changed, "move at rightmost boundary must return false");
        assert_eq!(engine.state.monitors[0].workspaces[0].columns.len(), 1);
    }

    // ─── B1: Viewport zoom and Overview are mutually exclusive ─────────────
    // Exact, deterministic float compares: the code under test drives the
    // field to exactly `1.0`, so no tolerance is needed.
    #[allow(clippy::float_cmp)]
    #[test]
    fn b1_viewport_then_overview_resets_viewport() {
        use crate::core::commands::{ToggleOverview, ViewportZoom};
        use crate::types::ViewportMode;
        let mut engine = setup_two_columns();
        engine.execute(ViewportZoom(1.0)); // page_zoom * 2 → enters Zoomed
        let ws = &engine.state.monitors[0].workspaces[0];
        assert_eq!(ws.viewport_mode, ViewportMode::Zoomed);
        assert!(ws.page_zoom_target > 1.0);
        assert!(!ws.overview, "viewport zoom must clear overview");
        // Toggling Overview while zoomed must drop the viewport state so the
        // two zoom axes can't fight over `alpha` (bug B1).
        engine.execute(ToggleOverview);
        let ws = &engine.state.monitors[0].workspaces[0];
        assert!(ws.overview);
        assert_eq!(
            ws.viewport_mode,
            ViewportMode::Normal,
            "overview must exit viewport zoom"
        );
        assert_eq!(
            ws.page_zoom_target, 1.0,
            "overview must reset page_zoom_target"
        );
    }

    // Exact, deterministic float compares: the code under test drives the
    // field to exactly `1.0`, so no tolerance is needed.
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
            ws.zoom_target, 1.0,
            "viewport zoom must reset the overview zoom_target"
        );
    }

    // Exact, deterministic float compares: the code under test drives the
    // field to exactly `1.0`, so no tolerance is needed.
    #[allow(clippy::float_cmp)]
    #[test]
    fn b1_viewport_zoom_does_not_corrupt_live_zoom() {
        use crate::core::commands::{ToggleOverview, ViewportZoom};
        use crate::types::ViewportMode;
        let mut engine = setup_two_columns();
        engine.execute(ViewportZoom(1.0));
        // While zoomed, overview's zoom_target must NOT silently ease the live
        // `zoom` spring (bug B1): the zoom axis stays at 1.0 until we exit zoom.
        engine.execute(ToggleOverview);
        let ws = &engine.state.monitors[0].workspaces[0];
        assert_eq!(ws.viewport_mode, ViewportMode::Normal);
        // settle the animation
        for _ in 0..200 {
            engine.state.tick_animations(1.0 / 60.0);
        }
        let ws = &engine.state.monitors[0].workspaces[0];
        assert!(
            (ws.zoom - ws.zoom_target).abs() < 0.01,
            "live zoom must track the overview target, not a phantom value"
        );
    }

    // ─── B2: Next/Prev keeps column.focused in sync with the target row ────
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
            boost: 1.0,
        });
        ws.columns.push(Column {
            windows: vec![20],
            focused: 0,
            weight: 0.5,
            boost: 1.0,
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

    // ─── Command system ─────────────────────────────────────────────────────
    // The typed command system is the unified entry point for keyboard, IPC
    // and tests. Each command is a pure transformation on State/Cfg producing
    // Effects. These tests exercise the new `Engine::execute` path directly.

    #[test]
    fn test_set_layout_command_emits_arrange() {
        let mut engine = setup_engine();
        let effects = engine.execute(crate::core::commands::SetLayout(LayoutKind::Grid));
        assert_eq!(
            engine.state.monitors[0].workspaces[0].layout,
            LayoutKind::Grid
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
    fn test_set_gaps_command_updates_cfg() {
        let mut engine = setup_engine();
        let before = engine.cfg.gaps_inner;
        engine.execute(crate::core::commands::SetGaps(
            crate::core::commands::GapKind::Inner,
            before + 10,
        ));
        assert_eq!(engine.cfg.gaps_inner, before + 10);
        assert_eq!(engine.cfg.gaps_outer, before, "Outer must be untouched");
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

    // ─── EventBus ───────────────────────────────────────────────────────────
    // The EventBus decouples producers (commands) from consumers (renderer,
    // IPC, bars, hooks, tests). A command declares its OWN domain event, never
    // its consumers.

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

        engine.execute(SetLayout(LayoutKind::Grid));
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
                boost: 1.0,
            });
            ws.focus = crate::types::Focus { column_idx: 0 };
        }
        engine
            .state
            .clients
            .insert(10, crate::types::Client::new(10, 0, 0));
        engine.state.monitors[0].focused = Some(10);

        let batch: Vec<Box<dyn crate::core::commands::Command>> = vec![
            Box::new(SetLayout(LayoutKind::Grid)),
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

    // ─── Capability Layer ──────────────────────────────────────────────────
    // External consumers (bars, hooks, tests) read through `Engine::query()`,
    // never by reaching into internal State/Client. Each query here must serve
    // several consumers — no "just in case" queries.

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
                boost: 1.0,
            });
            ws.focus = Focus { column_idx: 0 };
        }
        engine.state.monitors[0].focused = Some(42);
        engine.state.monitors[0].workspaces[0].layout = LayoutKind::Grid;
        engine
    }

    #[test]
    fn test_query_reports_focus_workspace_layout() {
        let engine = seed_engine_with_window();
        let q = engine.query();
        assert_eq!(q.focused_window(), Some(42));
        assert_eq!(q.active_workspace(), 0);
        assert_eq!(q.current_layout(), LayoutKind::Grid);
        assert_eq!(q.monitor_count(), 1);
        assert_eq!(q.workspace_count(), 9);
    }

    // Fase 8 observability: `query tree` must carry the non-semantic
    // desired/applied/real/focus/x11_focus/overlay/pending fields so a live
    // session can be audited end-to-end. These are read-only mirrors; this test
    // locks in their presence and that a focused seeded window reports
    // `focus:true`.
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

    // ─── Focus fallback con overlay (punto 5) ───────────────────────────────
    // Si un mosaico tapado por una overlay fullscreen (modo peek) se cierra,
    // `best_focus` debe devolver el foco a la ventana fullscreen de la overlay,
    // no a un mosaico invisible que quede debajo.

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
                boost: 1.0,
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
        // A fullscreen window is only an overlay in the `Grid` layout now (in
        // `Column` it joins the scrolling ribbon), so switch to Grid to keep
        // this overlay-preference assertion valid.
        engine.state.monitors[0].workspaces[0].layout = LayoutKind::Grid;
        // Focus history: 42 was peeked most recently, 7 was fullscreen before.
        engine.state.monitors[0].focus_stack = vec![7, 42];

        // Close 42 (the peeked tile) → 7 is still fullscreen →
        // best_focus must return 7, not a stale in-visible mosaic.
        assert_eq!(engine.state.best_focus(0), Some(7));

        // Without any overlay window, behavior stays column-focused.
        // Without any overlay window, behavior stays column-focused.
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
        let registry = default_registry();
        arrange(
            &engine.state,
            0,
            &engine.cfg,
            &registry,
            crate::core::layout::Phase::Live,
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
    fn test_grid_border_alignment() {
        use crate::core::layout::{arrange, Placements};
        use crate::types::Client;
        let mut engine = setup_engine();
        for i in 1..=4u32 {
            let mi = engine.state.sel_mon;
            let ws_i = engine.state.monitors[mi].active_ws;
            engine.state.monitors[mi].workspaces[ws_i].add_tiled(i, 0.5);
            let mut c = Client::new(i, mi, ws_i);
            c.border_w = engine.cfg.border_w;
            engine.state.add_client(c);
        }
        engine.state.monitors[0].workspaces[0].layout = LayoutKind::Grid;
        let cfg = &engine.cfg;
        // Grid now insets the workarea by `gaps_outer` on every edge (matching
        // the Column layout) and uses `(cols-1)`/`(rows-1)` inner gaps, so the
        // outer margin equals `gaps_outer`, not `gaps_inner` (N5).
        let gap = cfg.gaps_inner as i32;
        let gap_outer = cfg.gaps_outer as i32;
        let bw = cfg.border_w as i32;
        let n = 4usize;
        let wa = {
            let full = engine.state.monitors[0].workarea;
            Rect::new(
                full.x + gap_outer,
                full.y + gap_outer,
                full.w - (2 * gap_outer) as u32,
                full.h - (2 * gap_outer) as u32,
            )
        };
        let cols = (n as f64).sqrt().ceil() as i32;
        let rows = n.div_ceil(cols as usize) as i32;
        let cell_w = (wa.w as i32 - gap * (cols - 1)) / cols;
        let cell_h = (wa.h as i32 - gap * (rows - 1)) / rows;
        let expected = |c: i32, r: i32| -> Rect {
            Rect::new(
                wa.x + c * (cell_w + gap),
                wa.y + r * (cell_h + gap),
                (cell_w - 2 * bw).max(1) as u32,
                (cell_h - 2 * bw).max(1) as u32,
            )
        };
        let mut placements = Placements::new();
        let registry = default_registry();
        arrange(
            &engine.state,
            0,
            cfg,
            &registry,
            crate::core::layout::Phase::Live,
            &mut placements,
            &mut RibbonScratch::default(),
        );
        for (i, &(win, geom, _)) in placements.iter().enumerate() {
            let _ = win;
            let c = (i as i32) % cols;
            let r = (i as i32) / cols;
            assert_eq!(geom, expected(c, r), "grid cell {i} misaligned");
        }
        let left = expected(0, 0);
        let right = expected(1, 0);
        assert_eq!(right.x, left.x + left.w as i32 + 2 * bw + gap);
        let below = expected(0, 1);
        assert_eq!(below.y, left.y + left.h as i32 + 2 * bw + gap);
    }

    #[test]
    fn test_new_column_single_window_keeps_full_width() {
        // N3: `NewColumn` on a workspace with a single tiled window must leave
        // that window's (sole) column with weight ~1.0, not a sub-0.1 sliver
        // that the previous column-width fallback produced.
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
    fn test_grid_floats_clamped_to_workarea() {
        // N5: Grid's float branch must clamp floating geometry into the
        // workarea (matching the Column layout), so an off-screen float is
        // pulled back instead of being drawn at its raw unclamped coords.
        use crate::core::layout::{arrange, Placements};
        use crate::types::{Client, WinFlags};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].add_tiled(1, 0.5);
        let mut c = Client::new(1, mi, ws_i);
        c.border_w = engine.cfg.border_w;
        engine.state.add_client(c);

        let mut f = Client::new(2, mi, ws_i);
        f.border_w = engine.cfg.border_w;
        f.flags.set(WinFlags::FLOAT);
        f.geom = Rect::new(5000, 5000, 200, 200); // way off-screen
        engine.state.add_client(f);
        engine.state.monitors[mi].workspaces[ws_i].floats.push(2);

        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Grid;
        let mut placements = Placements::new();
        let registry = default_registry();
        arrange(
            &engine.state,
            mi,
            &engine.cfg,
            &registry,
            crate::core::layout::Phase::Live,
            &mut placements,
            &mut RibbonScratch::default(),
        );

        let (_, rect, _) = placements.iter().find(|e| e.0 == 2).copied().unwrap();
        let wa = engine.state.monitors[mi].workarea;
        assert!(
            rect.x + rect.w as i32 <= wa.x + wa.w as i32,
            "float must be clamped within the workarea horizontally"
        );
        assert!(
            rect.y + rect.h as i32 <= wa.y + wa.h as i32,
            "float must be clamped within the workarea vertically"
        );
    }

    #[test]
    fn test_fullscreen_unfocused_layering() {
        use crate::core::layout::{arrange, LayoutRegistry, Placements};
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
        let registry = LayoutRegistry::new();
        arrange(
            &engine.state,
            0,
            &engine.cfg,
            &registry,
            crate::core::layout::Phase::Live,
            &mut p,
            &mut RibbonScratch::default(),
        );
        let raised = present(&engine.state, &engine.state.monitors[0], &mut p);

        assert!(
            !raised.contains(&1),
            "unfocused maximized must not bleed into overlay"
        );
        let (_, rect1, _) = p.iter().find(|e| e.0 == 1).copied().unwrap();
        assert!(rect1.w < engine.state.monitors[0].workarea.w);
    }

    #[test]
    fn test_focus_direction_allowed_in_fullscreen() {
        // 0.18.2 behavior, restored: FocusDirection is never gated on the
        // focused window's fullscreen flag. Moving focus away from a
        // fullscreen window is exactly what puts `core::present`'s peek mode
        // to use — the overlay stays put (see `fullscreen_persists_while_unfocused`
        // in present.rs) while the newly focused tile renders above it.
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
                boost: 1.0,
            });
            ws.columns.push(Column {
                windows: vec![2],
                focused: 0,
                weight: 1.0,
                boost: 1.0,
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
        let cam = ws.camera.target;
        let mut p = Placements::new();
        let registry = LayoutRegistry::new();
        engine.state.monitors[mi].workspaces[ws_i].camera.position = cam;
        crate::core::layout::arrange(
            &engine.state,
            mi,
            &engine.cfg,
            &registry,
            crate::core::layout::Phase::Live,
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
        // 0.18.2 behavior, restored: MoveWindow is never gated on fullscreen.
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
                boost: 1.0,
            });
            ws.columns.push(Column {
                windows: vec![2],
                focused: 0,
                weight: 1.0,
                boost: 1.0,
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

        // The fullscreen window is a RIBBON participant: moving it relocates its
        // column (here to index 1) rather than re-pinning an overlay.
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
        let cam = ws.camera.target;
        let mut p = Placements::new();
        let registry = LayoutRegistry::new();
        engine.state.monitors[mi].workspaces[ws_i].camera.position = cam;
        crate::core::layout::arrange(
            &engine.state,
            mi,
            &engine.cfg,
            &registry,
            crate::core::layout::Phase::Live,
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

    // ─── Grid: focus and geometry stay consistent (plan 1786499080900) ────────
    //
    // A spatial `FocusDirection` must move focus to the window that is actually
    // to the left/right/up/down on screen, and a subsequent `arrange` must place
    // that focused window at the matching geometric cell.

    #[test]
    fn focus_and_arrange_are_consistent() {
        use crate::core::commands::FocusDirection;
        use crate::core::grid;
        use crate::core::layout::{arrange, LayoutRegistry, Placements, RibbonScratch};
        use crate::types::{Client, Dir};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        for i in 1..=4u32 {
            engine.state.monitors[mi].workspaces[ws_i].add_tiled(i, 0.5);
            let mut c = Client::new(i, mi, ws_i);
            c.border_w = engine.cfg.border_w;
            engine.state.add_client(c);
        }
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Grid;
        engine.state.monitors[mi].focused = Some(1);
        engine.state.monitors[mi].focus_stack = vec![1, 2, 3, 4];

        // Populate the grid snapshot the way the render path does, so the focus
        // command navigates the real geometry.
        let (_, snap) = grid::arrange_workspace(
            &engine.state.monitors[mi].workspaces[ws_i],
            &engine.cfg,
            &engine.state.monitors[mi],
            None,
        );
        engine.state.monitors[mi].workspaces[ws_i].grid_snapshot = Some(snap);

        engine.execute(FocusDirection(Dir::Right));
        let focused = engine.state.monitors[mi].focused.expect("focus is set");
        assert_ne!(focused, 1, "Right must move focus off window 1");

        let mut p = Placements::new();
        let registry = LayoutRegistry::new();
        arrange(
            &engine.state,
            mi,
            &engine.cfg,
            &registry,
            crate::core::layout::Phase::Live,
            &mut p,
            &mut RibbonScratch::default(),
        );
        let frect = p.iter().find(|e| e.0 == focused).unwrap().1;
        let w1rect = p.iter().find(|e| e.0 == 1).unwrap().1;
        assert!(
            frect.x > w1rect.x,
            "focused window sits to the right of window 1 after FocusDirection(Right)"
        );
    }

    #[test]
    fn grid_move_swaps_with_geometric_neighbour() {
        use crate::core::commands::MoveWindow;
        use crate::core::grid;
        use crate::core::layout::{arrange, LayoutRegistry, Placements, RibbonScratch};
        use crate::types::{Client, Dir};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        for i in 1..=4u32 {
            engine.state.monitors[mi].workspaces[ws_i].add_tiled(i, 0.5);
            let mut c = Client::new(i, mi, ws_i);
            c.border_w = engine.cfg.border_w;
            engine.state.add_client(c);
        }
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Grid;
        engine.state.monitors[mi].focused = Some(1);
        let (_, snap) = grid::arrange_workspace(
            &engine.state.monitors[mi].workspaces[ws_i],
            &engine.cfg,
            &engine.state.monitors[mi],
            None,
        );
        engine.state.monitors[mi].workspaces[ws_i].grid_snapshot = Some(snap);

        // Move window 1 to the right: it should swap places with its right
        // neighbour in the flat window order, and a re-arrange must reflect it.
        engine.execute(MoveWindow(1, Dir::Right));
        let mut p = Placements::new();
        let registry = LayoutRegistry::new();
        arrange(
            &engine.state,
            mi,
            &engine.cfg,
            &registry,
            crate::core::layout::Phase::Live,
            &mut p,
            &mut RibbonScratch::default(),
        );
        let order: Vec<WindowId> = p.iter().map(|e| e.0).collect();
        // Window 1 is no longer the first in the flat order (it swapped right).
        assert_ne!(order[0], 1, "MoveWindow(Right) reordered the grid");
        assert!(
            order.contains(&1) && order.contains(&2),
            "both windows still tiled after the swap"
        );
    }

    // ─── Grid fullscreen roundtrip does not destroy grid state ───────────────
    //
    // The fullscreen overlay (present.rs) is independent of the base grid
    // geometry, so the A/B scenario — A fullscreen, create B, fullscreen B,
    // exit B — must leave A fullscreen and B restored to its grid tile.

    #[test]
    fn fullscreen_roundtrip_restores_grid() {
        use crate::core::layout::{arrange, LayoutRegistry, Placements, RibbonScratch};
        use crate::core::present::present;
        use crate::types::{Client, WinFlags};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Grid;

        engine.state.monitors[mi].workspaces[ws_i].add_tiled(1, 0.5);
        let mut a = Client::new(1, mi, ws_i);
        a.border_w = engine.cfg.border_w;
        engine.state.add_client(a);
        engine.state.monitors[mi].workspaces[ws_i].add_tiled(2, 0.5);
        let mut b = Client::new(2, mi, ws_i);
        b.border_w = engine.cfg.border_w;
        engine.state.add_client(b);
        engine.state.monitors[mi].focused = Some(2);
        engine.state.monitors[mi].focus_stack = vec![1, 2];

        // A fullscreen (flag set directly, as the backend SetFullscreen effect
        // would); then create+fullscreen B too. Both stay tiled in the grid —
        // the overlay is presentation-only.
        engine
            .state
            .clients
            .get_mut(&1)
            .unwrap()
            .flags
            .set(WinFlags::FULLSCREEN);
        engine
            .state
            .clients
            .get_mut(&2)
            .unwrap()
            .flags
            .set(WinFlags::FULLSCREEN);

        let tiled: Vec<WindowId> = engine.state.monitors[mi].workspaces[ws_i]
            .columns
            .iter()
            .flat_map(|c| c.windows.iter().copied())
            .collect();
        assert!(
            tiled.contains(&1) && tiled.contains(&2),
            "both windows tiled while fullscreen"
        );

        // Exit B's fullscreen (clear its flag). A must remain fullscreen and the
        // grid state must not be destroyed/normalized.
        engine
            .state
            .clients
            .get_mut(&2)
            .unwrap()
            .flags
            .clear(WinFlags::FULLSCREEN);

        // `best_focus` must still prefer the surviving fullscreen overlay (A).
        assert_eq!(
            engine.state.best_focus(mi),
            Some(1),
            "best_focus keeps A (still fullscreen) after B exits"
        );
        assert!(
            engine.state.clients.get(&1).unwrap().is_fullscreen(),
            "A keeps its fullscreen flag"
        );

        let mut p = Placements::new();
        let registry = LayoutRegistry::new();
        arrange(
            &engine.state,
            mi,
            &engine.cfg,
            &registry,
            crate::core::layout::Phase::Live,
            &mut p,
            &mut RibbonScratch::default(),
        );
        // Apply the presentation overlay (fullscreen) as the render path does.
        present(&engine.state, &engine.state.monitors[mi], &mut p);
        let (_, arect, abw) = p.iter().find(|e| e.0 == 1).unwrap();
        assert_eq!(
            *abw, 0,
            "A still presented as a borderless fullscreen overlay"
        );
        assert_eq!(
            *arect, engine.state.monitors[mi].screen,
            "A fills the screen"
        );
        let (_, brect, _) = p.iter().find(|e| e.0 == 2).unwrap();
        assert!(
            brect.w < engine.state.monitors[mi].screen.w,
            "B was restored to a normal grid tile, not fullscreen"
        );
        let tiled2: Vec<WindowId> = engine.state.monitors[mi].workspaces[ws_i]
            .columns
            .iter()
            .flat_map(|c| c.windows.iter().copied())
            .collect();
        assert!(
            tiled2.contains(&1) && tiled2.contains(&2),
            "grid not destroyed after the fullscreen roundtrip"
        );
    }

    // ─── Scroll-camera desync regression (plan 1786124999628) ───────────────
    //
    // The camera must keep the focused column fully on-screen for every column
    // count and every focus position — the exact symptom that was reported
    // ("after 3 tiles the camera loses the other tile").

    /// Build a workspace on monitor 0 with `n` single-window columns of weight
    /// `[1.0, 0.6, 0.6, …]`, focused at `focus_ci`. `overview` drives the
    /// Overview (zoom-out) state. Per-column `boost` is forced to its settled
    /// steady-state so `arrange` and `ideal_scroll` agree (the focused column
    /// is boosted, the rest are at rest; in Overview every column is at rest).
    fn build_ribbon(n: usize, focus_ci: usize, overview: bool) -> Engine {
        use crate::types::{Client, Column, Focus};
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        {
            let ws = &mut engine.state.monitors[mi].workspaces[0];
            let weights: Vec<f32> = (0..n).map(|i| if i == 0 { 1.0 } else { 0.6 }).collect();
            for (i, w) in weights.iter().enumerate() {
                let win = (i + 1) as u32;
                let boost_val = if overview {
                    0.0
                } else if i == focus_ci {
                    1.0
                } else {
                    0.0
                };
                ws.columns.push(Column {
                    windows: vec![win],
                    focused: 0,
                    weight: *w,
                    boost: boost_val,
                });
            }
            ws.focus = Focus {
                column_idx: focus_ci,
            };
            if overview {
                ws.overview = true;
                ws.zoom = 0.25;
                ws.zoom_target = 0.25;
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
                let registry = default_registry();
                arrange(
                    &engine.state,
                    mi,
                    &cfg,
                    &registry,
                    crate::core::layout::Phase::Live,
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
                // Bug #1: the focused column must never be wider than the
                // visible area (was 2465px on a 1920 screen for column 0).
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
                let registry = default_registry();
                arrange(
                    &engine.state,
                    mi,
                    &cfg,
                    &registry,
                    crate::core::layout::Phase::Live,
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
                let registry = default_registry();
                arrange(
                    &engine.state,
                    mi,
                    &cfg,
                    &registry,
                    crate::core::layout::Phase::Live,
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
        let registry = default_registry();
        arrange(
            &engine.state,
            mi,
            &cfg,
            &registry,
            crate::core::layout::Phase::Live,
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

    // ─── Bug #1: FocusDirection(Next/Prev) must move the camera ────────────────
    //
    // Next/Prev navigates the focus *stack* (not the column grid), so it used to
    // update `mon.focused` without syncing `ws.focus.column_idx`. `ideal_scroll`
    // reads `focus.column_idx`, so the camera stayed on the old column and the
    // now-focused window could scroll off-screen. The fix re-derives the column
    // from the target window before recomputing the scroll.

    #[test]
    fn focus_direction_next_prev_syncs_column_and_camera() {
        use crate::core::commands::FocusDirection;
        use crate::core::layout::{arrange, ideal_scroll, Placements};
        use crate::types::{Client, Column, Dir, Focus};
        let cfg = default_cfg();
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        // Three narrow columns so the ribbon is wider than the screen: centering
        // one column pushes the others partially off-screen, which is exactly
        // the condition that exposed the desync.
        {
            let ws = &mut engine.state.monitors[mi].workspaces[0];
            for i in 1..=3u32 {
                ws.columns.push(Column {
                    windows: vec![i],
                    focused: 0,
                    weight: 0.4,
                    boost: 1.0,
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
            &default_registry(),
            crate::core::layout::Phase::Live,
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

    // ─── Bug #3: drop-to-tile must update the target column's focused row ──────

    #[test]
    fn drop_into_column_sets_focused_row() {
        use crate::types::{Column, Focus, Workspace};
        let mut ws = Workspace::new(0);
        ws.columns.push(Column {
            windows: vec![10, 20],
            focused: 0,
            weight: 0.5,
            boost: 1.0,
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

    // ─── B1: reload with fewer tags must clamp client workspaces ──────────────
    //
    // `reload_config` reconciles each monitor's workspaces to the new `n_tags`
    // and clamps any client whose `workspace >= n_tags`. The backend republishes
    // the EWMH desktop count/names (and per-client `_NET_WM_DESKTOP`) for exactly
    // those clamped clients. This unit test guards the core invariant behind that
    // handoff: after a reload that shrinks the tag count, every client's
    // `workspace` stays strictly below `n_tags` and no monitor keeps stale slots.

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
                    boost: 1.0,
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

    // ─── P1: CollapseColumn must absorb the collapsed column's width ──────────
    //
    // `retain` drops the emptied column and `rebalance_weights` only repairs
    // weights <= 0 (it never re-normalizes), so if the collapsed column's weight
    // isn't handed to the target the ribbon permanently loses that much width
    // and leaves an empty gap on the right of the workarea.

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
                    boost: 1.0,
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

    // ─── P2: horizontal focus must keep the row you were on ───────────────────
    //
    // Up/Down tracks the row by writing `col.focused`; Left/Right used to only
    // move `focus.column_idx` and then read the destination column's own (stale)
    // `focused`, so focus jumped to an unrelated window instead of the neighbour.

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
                boost: 1.0,
            });
            ws.columns.push(Column {
                windows: vec![4, 5, 6],
                focused: 0, // stale: never visited
                weight: 0.4,
                boost: 1.0,
            });
            ws.columns.push(Column {
                windows: vec![7], // shorter than the row we come from
                focused: 0,
                weight: 0.4,
                boost: 1.0,
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

    // ─── P4: best_focus must mirror `core::present`'s overlay rule ────────────
    //
    // `present` only presents a maximized window while it is `mon.focused`, so a
    // maximized window sitting in the background must not be `best_focus`'s top
    // candidate either — otherwise viewing a workspace hands it the focus and it
    // immediately blows up to fill the workarea.

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
                boost: 1.0,
            });
            ws.columns.push(Column {
                windows: vec![2],
                focused: 0,
                weight: 0.5,
                boost: 1.0,
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

        // A fullscreen window is still an overlay even while unfocused — but
        // only in the `Grid` layout (in `Column` it joins the ribbon). Switch to
        // Grid so this assertion stays valid. It covers the screen regardless of
        // focus, so focus must return to it.
        let c = engine.state.clients.get_mut(&1).unwrap();
        c.flags.clear(WinFlags::MAXIMIZED);
        c.flags.set(WinFlags::FULLSCREEN);
        engine.state.monitors[mi].workspaces[0].layout = LayoutKind::Grid;
        assert_eq!(engine.state.best_focus(mi), Some(1));
    }

    // ─── Bug: workspace switch must keep the focused window Maverick considers
    //     focused (lost keyboard focus on return) ──────────────────────────────
    //
    // Repro of the reported bug: Alacritty focused on ws0, switch to ws1, switch
    // back to ws0 — Alacritty is visible again but the real X input focus is gone
    // until `h`/`l` is pressed. The state-level invariant this test locks: the
    // window Maverick *considers* focused (`best_focus`, which `ViewWorkspace`
    // uses to pick its `FocusWindow` target) must survive the trip away and back,
    // and `ViewWorkspace` must keep emitting `FocusWindow` for that same window.
    //
    // The actual desync is in the X11 backend: `Backend::focus` set the real X
    // input focus and then ran `reconcile_focus()` *before* committing the
    // logical `mon.focused`, so when a command (like `ViewWorkspace`) did not
    // pre-write `mon.focused` the reconcile re-asserted focus onto the
    // previously-focused, now-hidden window (fixed in `backend/x11/render.rs` by
    // committing `mon.focused` before `reconcile_focus`). This state/effect test
    // guards the core contract that fix depends on; the X-level reconciliation
    // itself is validated under Xephyr via the `input-trace` diagnostics.
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
                boost: 1.0,
            });
            ws.columns.push(Column {
                windows: vec![2],
                focused: 0,
                weight: 0.5,
                boost: 1.0,
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
                boost: 1.0,
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
    // ── GrowColumn clamp panic regression (bug C2) ──────────────────────────────
    //
    // With many columns the old `1.0 - 0.05*(n-1)` upper bound drops below the
    // `0.05` lower bound of the `.clamp`, so `f32::clamp`'s `min <= max` assert
    // panicked (in debug *and* release) on `GrowCol`. Assert the command runs
    // without panicking even with 25 columns.

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
        // Grow both directions; previously panicked once 21+ columns were present.
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

    // ─── Fullscreen-as-ribbon-regression (plan 1786166283911) ────────────────────
    //
    // The invariant: `ribbon_geom` is the single source of truth shared by
    // `arrange_columns`, `ideal_scroll` and `column_screen_extents`. A fullscreen
    // column must feed its special width through `ribbon_geom` so all three agree.
    // This mirrors `layout.rs`'s `ribbon_invariants_hold_with_fullscreen` at the
    // higher-level `Engine`/`arrange` boundary.

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
                boost: 1.0,
            });
            ws.columns.push(Column {
                windows: vec![2],
                focused: 0,
                weight: 0.5,
                boost: 0.0,
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
        let registry = default_registry();
        arrange(
            &engine.state,
            mi,
            &cfg,
            &registry,
            crate::core::layout::Phase::Live,
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

    // ─── ToggleFullscreen moves a float in/out of the tiling (plan 1786166283911) ─
    //
    // Entering fullscreen from a float pulls the window into the tiling (as a fresh
    // column) and remembers it was floating; leaving fullscreen returns it to its
    // float. The core command owns this topology change AND the FULLSCREEN flag;
    // the backend's `SetFullscreen` handler is now X11-only (EWMH atom + bypass
    // hint) and must not mutate logical state.

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

    // ─── Fullscreen target resolves from the logically-focused window ───────────
    //
    // Bug (plan 1786493542516): when B is created while A is a fullscreen/
    // maximized overlay, manage() must advance the *logical* focus to B (without
    // moving X input focus off the overlay). Keyboard actions resolve from
    // `mon.focused`, so `Mod4+F` must target B, not the overlay A. manage()
    // itself needs a real X11 connection, so the scenario is reproduced here at
    // the command layer: A fullscreen + X-input-focused, B tiled with the
    // logical focus advanced to B (exactly what the fixed manage() does).
    //
    // The `FULLSCREEN` flag is owned by the `ToggleFullscreen` Command, so we
    // assert on the *target* window the command emits (the flag is already set
    // by the command; no backend simulation needed).

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

        // B is created and tiled under the overlay. Per the managed-window
        // policy (plan 1786493542516 §E), manage() advances the *logical*
        // focus to B while leaving the X input focus on the overlay A.
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
        // The Command already set B's FULLSCREEN flag.
        assert!(
            engine.state.clients.get(&a).unwrap().is_fullscreen(),
            "the overlay A must keep its fullscreen"
        );
        assert_eq!(engine.state.monitors[mi].focused, Some(b));
    }

    #[test]
    fn toggle_fullscreen_stale_focus_targets_overlay_not_new() {
        // Documents the root-cause coupling this fix closes: when the logical
        // focus is NOT advanced to the newly-created B (the old divergent
        // state), `ToggleFullscreen(None)` resolves from `mon.focused` (= A) and
        // so it un-fullscreens the overlay instead of targeting B. This is the
        // exact bug; the manage() fix advances logical focus to B to avoid it.
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
        // Divergent (pre-fix) state: logical focus stays on A, column pointer
        // already moved to B by add_tiled.
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

    // ─── Fase 1: the EWMH fullscreen path must promote a float too (bug C1/A1) ────
    //
    // The keyboard and EWMH paths both go through `ToggleFullscreen`, which owns
    // the `FULLSCREEN` flag and the float→tiling promotion together via
    // `apply_fullscreen_topology`. The EWMH path used to skip the promotion
    // entirely: a float — mpv is the canonical case — stayed in `ws.floats`, was
    // laid out from `client.geom`, and the old `Rect::default()` sentinel
    // collapsed it to 0×0 (bug C1/A1). This test exercises the shared topology
    // helper directly:

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
            &default_registry(),
            crate::core::layout::Phase::Live,
            &mut p,
            &mut RibbonScratch::default(),
        );

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

    // ─── Fase 0 (plan 1786564084575): fullscreen restore must be EXACT ──────────
    //
    // The single `saved_geom: Rect` that used to remember the pre-fullscreen
    // geometry is fragile: `set_maximized` also writes it, so a window
    // maximized *while fullscreen* clobbers the float rect, and leaving
    // fullscreen then restores the wrong geometry. The fix (Fase 3) captures a
    // `FullscreenSnapshot { prior mode, exact rect }` on enter and restores it
    // verbatim on leave. These tests are the anchor for that contract.

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

        // While fullscreen, the window is ALSO maximized. The old code wrote
        // `saved_geom = geom` here, clobbering the float rect (the bug).
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
        // The plan's named scenario: A fullscreen + create B + fullscreen B,
        // then B leaves and A leaves — A must return to its exact pre-fullscreen
        // rect and topology, never collapsing or stealing B's geometry.
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

        // The Command runs it once; the EWMH path would call it too, but the
        // backend no longer does. Either way the second call must change nothing.
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
            ws.page_zoom_target > 1.0,
            "page_zoom target must grow past 1.0"
        );

        // The live `page_zoom` is an animated spring (Fase 11); advance it so
        // `ribbon_geom` reads the enlarged factor.
        for _ in 0..40 {
            engine.state.tick_animations(1.0 / 60.0);
        }
        let ws = &engine.state.monitors[mi].workspaces[ws_i];
        assert!(ws.page_zoom > 1.0, "page_zoom spring must ease past 1.0");

        // `ribbon_geom` must feed the viewport factor into `alpha` so columns
        // are enlarged (alpha > 1), independent of the Overview zoom.
        let wa = engine.state.monitors[mi].workarea;
        let fs = fs_ctx(&engine.state.clients, ws, engine.state.monitors[mi].screen);
        let g = ribbon_geom(ws, &engine.cfg, wa, true, &fs);
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
        assert!((ws.page_zoom_target - 1.0).abs() < 1e-6);
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
        engine.state.monitors[mi].workspaces[ws_i].camera.target = 0.0;
        let before = engine.state.monitors[mi].workspaces[ws_i].camera.target;
        engine.dispatch(Action::PageSnap(Dir::Right));
        let after = engine.state.monitors[mi].workspaces[ws_i].camera.target;
        let wa = engine.state.monitors[mi].workarea;
        let expected_step = wa.w as f32; // alpha = 1.0 → one screen-width page
        assert!(
            after > before && after <= expected_step + 1.0,
            "PageSnap right must scroll the camera forward by one page (~{expected_step}): got {after}"
        );
    }

    // ─── Fase 5 (plan 1786564084575): property-based invariant harness ─────────
    //
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
            CollapseColumn, Command, CycleLayout, FocusDirection, FocusMonitor, GrowColumn,
            NewColumn, OverviewNav, SetLayout, ToggleFloat, ToggleFullscreen, ToggleMaximize,
            ToggleOverview,
        };
        use crate::core::effect::Effect;
        use crate::core::layout::{arrange, Phase, Placements, RibbonScratch};
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
                    run_cmd(&mut engine, ToggleFloat);
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
                    run_cmd(&mut engine, CycleLayout);
                }
                16 => {
                    let lk = if rng.below(2) == 0 {
                        LayoutKind::Column
                    } else {
                        LayoutKind::Grid
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
                    &default_registry(),
                    Phase::Settled,
                    &mut p1,
                    &mut r1,
                );
                arrange(
                    &engine.state,
                    mi,
                    &engine.cfg,
                    &default_registry(),
                    Phase::Settled,
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

        // Layout must still be valid/deterministic at the end.
        engine
            .state
            .check_invariants()
            .expect("final state must satisfy invariants");
    }

    // ─── Canonical overlay predicate + focus / pending-focus suite ────────────
    //
    // `State::presented_overlay_owner` is the SINGLE source of truth for "who
    // owns the presented overlay": a fullscreen window only counts in `Grid` (or
    // under `FullscreenPolicy::True`) — a `Column`-layout Normal fullscreen is
    // just a ribbon tile — plus the *focused* maximized window
    // (`presented_maximize`). The helpers below mirror the backend paths that
    // consume it (`manage`, `unmanage`, `focus`) so the tests exercise the same
    // decisions without an X server.

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

    // 1.
    #[test]
    fn fullscreen_column_normal_new_window_receives_focus() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Column;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);

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

    // 2.
    #[test]
    fn fullscreen_grid_or_true_keeps_overlay() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        t_manage(&mut engine, 1);
        t_set_fullscreen(&mut engine, 1, true);

        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Grid;
        assert_eq!(
            engine.state.presented_overlay_owner(mi),
            Some(1),
            "a Grid fullscreen IS the presented overlay"
        );

        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Column;
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

    // 3.
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

    // 4.
    #[test]
    fn fullscreen_a_create_b_destroy_b_focus_returns_to_a() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Grid;
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

    // 5.
    #[test]
    fn fullscreen_a_create_b_focus_b_does_not_hijack_a() {
        use crate::core::effect::Effect;
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Grid;
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

    // 6.
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

    // 7.
    #[test]
    fn workspace_switch_does_not_steal_focus_via_pending() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        engine.state.monitors[mi].workspaces[0].layout = LayoutKind::Grid;
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

    // 8.
    #[test]
    fn focus_fullscreen_create_destroy_never_leaves_invalid_focus() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        engine.state.monitors[mi].workspaces[0].layout = LayoutKind::Grid;
        t_manage(&mut engine, 1);
        t_manage(&mut engine, 2);
        t_focus(&mut engine, 1);

        engine.execute(crate::core::commands::ToggleFullscreen(Some(1)));
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

    // 9.
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
                                && (c.is_fullscreen() && (ws.layout == LayoutKind::Grid || c.is_true_fullscreen())
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
                    engine.execute(ToggleFloat);
                }
                // Manage a window on a randomly-selected monitor/workspace so the
                // deferral can be bound to a monitor/workspace that is NOT the
                // selected one — this is what previously orphaned deferred windows.
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

    // 10.
    #[test]
    fn pending_focus_consumed_on_fullscreen_keyboard_dismiss() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Grid;
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

    // 11.
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

    // 12.
    #[test]
    fn pending_focus_invalidated_when_deferred_window_gone() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Grid;
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

    // 13.
    #[test]
    fn destroy_overlay_owner_consumes_pending() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Grid;
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

    // 13b. Orphan fix — scenario 4: an overlay destroyed on a NON-active
    // workspace (selected monitor) must still hand focus to the deferred window
    // instead of orphaning it.
    #[test]
    fn orphan_defer_not_lost_when_overlay_destroyed_on_non_active_ws() {
        let mut engine = setup_engine_multi();
        let mon0 = 0;
        engine.state.monitors[mon0].workspaces[0].layout = LayoutKind::Grid;
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

    // 13c. Orphan fix — scenario 8: a pending deferral created on mon0/ws0 must be
    // consumed when the overlay is dismissed after a monitor+workspace switch,
    // i.e. when the teardown happens on a different selected monitor/workspace
    // than when the deferral was created.
    #[test]
    fn orphan_defer_not_lost_when_ws_switch_then_dismiss_on_other_ws() {
        use crate::core::effect::Effect;
        let mut engine = setup_engine_multi();
        let mon0 = 0;
        engine.state.monitors[mon0].workspaces[0].layout = LayoutKind::Grid;
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

    // 13d. Orphan fix — scenario 9: an overlay destroyed on a NON-selected monitor
    // must still hand focus to the deferred window on that monitor.
    #[test]
    fn orphan_defer_not_lost_when_overlay_destroyed_on_non_selected_monitor() {
        let mut engine = setup_engine_multi();
        let mon0 = 0;
        engine.state.monitors[mon0].workspaces[0].layout = LayoutKind::Grid;
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

    // 1.4 (a). A fullscreen Grid overlay dismissed by a layout change (SetLayout)
    // must hand focus to the deferred window, with no orphan / no #8c violation.
    #[test]
    fn pending_focus_resolved_when_fullscreen_overlay_lost_on_setlayout() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Grid;
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

        engine.execute(crate::core::commands::SetLayout(
            crate::types::LayoutKind::Column,
        ));
        assert_eq!(
            engine.state.monitors[mi].focused,
            Some(2),
            "SetLayout that drops the Grid overlay must hand input to the deferred window"
        );
        assert!(
            engine.state.pending_focus.is_none(),
            "the deferral is resolved exactly once"
        );
        engine
            .state
            .check_invariants()
            .expect("SetLayout dismiss must preserve invariants");
    }

    // 1.4 (b). CycleLayout (Grid -> Column) dismisses the overlay the same way.
    #[test]
    fn pending_focus_resolved_when_fullscreen_overlay_lost_on_cyclelayout() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Grid;
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

        engine.execute(crate::core::commands::CycleLayout);
        assert_eq!(
            engine.state.monitors[mi].focused,
            Some(2),
            "CycleLayout that drops the Grid overlay must hand input to the deferred window"
        );
        assert!(
            engine.state.pending_focus.is_none(),
            "the deferral is resolved exactly once"
        );
        engine
            .state
            .check_invariants()
            .expect("CycleLayout dismiss must preserve invariants");
    }

    // 1.4 (c). Moving the overlay owner to another workspace dismisses it.
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
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Grid;
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

    // 1.4 (d). Moving the overlay owner to another monitor dismisses it.
    #[test]
    fn pending_focus_resolved_when_overlay_owner_moved_to_other_mon() {
        let mut engine = setup_engine_multi();
        let mon0 = 0;
        let ws_i = engine.state.monitors[mon0].active_ws;
        engine.state.monitors[mon0].workspaces[ws_i].layout = LayoutKind::Grid;
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

    // 1.4 (e). NEGATIVE: a deferral whose overlay lives on a now-hidden workspace
    // must SURVIVE a workspace switch (the overlay is merely hidden, not dismissed).
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
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Grid;
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

    // 1.4 (f). NEGATIVE: a deferral bound to a different (now non-selected) monitor
    // must SURVIVE a monitor switch — its overlay is still presented there.
    #[test]
    fn pending_focus_survives_when_overlay_on_non_selected_monitor() {
        let mut engine = setup_engine_multi();
        let mon0 = 0;
        let ws_i = engine.state.monitors[mon0].active_ws;
        engine.state.monitors[mon0].workspaces[ws_i].layout = LayoutKind::Grid;
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

    // 14.
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

    // 15.
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

    // ─── Wave 3 (Phase 10/11): geometry pipeline + reconcile contract ────────
    //
    // These drive the pure `arrange` + `present_into` + `DesiredState::from_placements`
    // + `reconcile` pipeline (no X server) and pin the geometry each window
    // *should* receive for every state the WM produces. `reconcile` is the single
    // owner of "what has actually been written to X11" and these tests assert the
    // Desired it is diffed against is exactly the layout/present projection.

    /// Run the production geometry pipeline for monitor `mi` and return the
    /// explicit `DesiredState` (exactly what `reconcile` is later diffed against).
    fn pipeline_desired(engine: &Engine, mi: usize) -> DesiredState {
        use crate::core::layout::{arrange, LayoutRegistry, Phase, Placements, RibbonScratch};
        use crate::core::present::present_into;
        let mut placements = Placements::new();
        let registry = LayoutRegistry::new();
        arrange(
            &engine.state,
            mi,
            &engine.cfg,
            &registry,
            Phase::Settled,
            &mut placements,
            &mut RibbonScratch::default(),
        );
        let mut raise = Vec::new();
        present_into(
            &engine.state,
            &engine.state.monitors[mi],
            &mut placements,
            &mut raise,
        );
        DesiredState::from_placements(&placements, &raise)
    }

    // 5. A fullscreen (Grid) window's desired geometry is the whole screen.
    #[test]
    fn overlay_desired_geometry_matches_layout() {
        use crate::types::LayoutKind;
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Grid;
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

    // 6. Fullscreen desired geometry equals the monitor screen (multi-window).
    #[test]
    fn fullscreen_desired_geometry() {
        use crate::types::LayoutKind;
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Grid;
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

    // 7. A maximized window's desired geometry equals the workarea (border 0).
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

    // 8. A floating window's desired geometry equals its client.geom.
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

    // 9. A tiled window's self-resize request is DENIED: client.geom (the desired)
    //    stays the WM-authored tile, never the client's divergent request.
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
            &default_registry(),
            crate::core::layout::Phase::Settled,
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
        // must NOT adopt it: classify_configure returns Diverged{follow:false}, so
        // the model re-asserts Desired instead of mutating client.geom.
        let requested = Rect::new(40, 40, 640, 480);
        assert_ne!(
            requested, before,
            "sanity: the request differs from the tile"
        );
        let applied = AppliedWindow {
            rect: before,
            border_w: 2,
            seen: true,
        };
        let c = engine.state.clients.get(&1).unwrap();
        let obs = classify_configure(requested, 2, &applied, c);
        assert!(
            matches!(obs, ConfigureObservation::Diverged { follow: false }),
            "tiled self-resize must be denied (re-asserted), not followed"
        );
        assert_eq!(
            engine.state.clients[&1].geom, before,
            "client.geom (desired) is unchanged after the self-resize request"
        );
    }

    // 10. A tiled window that diverges (geometry_dirty set) must re-apply: the
    //     next pipeline run's reconcile returns a Configure for it.
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

    // 11. A float that self-resizes is followed: the model adopts the requested
    //     rect, and the pipeline's Desired for that window matches client.geom.
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

        // The convergence policy agrees: a float's divergence is followed.
        let applied = AppliedWindow {
            rect: g0,
            border_w: 2,
            seen: true,
        };
        let c = engine.state.clients.get(&1).unwrap();
        let obs = classify_configure(g1, 2, &applied, c);
        assert!(
            matches!(obs, ConfigureObservation::Diverged { follow: true }),
            "a float's self-resize must be followed, not re-asserted"
        );
    }

    // ─── Phase 11: property test — geometry pipeline stays consistent ───────
    //
    // Mirror `property_random_window_ops_preserve_invariants` but assert on the
    // *geometry* contract across a randomized Create/Destroy/Fullscreen/Maximize/
    // Float/MoveResize/WorkspaceSwitch/MonitorSwitch/LayoutChange chaos. After
    // every step we build the explicit Desired for every monitor, diff it against
    // a single long-lived AppliedState via `reconcile`, and assert the invariants
    // that the backend relies on to never write a bogus Configure to X11.

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
                all.raise.extend(d.raise);
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
                    engine.execute(ToggleFloat);
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
                        LayoutKind::Grid
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
                    && (ws.layout == LayoutKind::Grid || c.is_true_fullscreen()))
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

    // ─── AUDITORÍA DE RESISTENCIA REAL (fases 1–5 y 9) ───────────────────────
    //
    // Audit-only tests. They drive the existing production paths (Engine::execute,
    // t_manage/t_destroy/t_set_fullscreen/t_set_maximized, pipeline_desired, the
    // reconciler `reconcile`/`classify_configure`) and assert invariants. They
    // NEVER modify core WM code. Any test that reveals wrong behaviour is marked
    // `#[ignore]` with the panic text and reported as a FOUND BUG.

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

    // ─── Phase 1 — hostile-client behaviour matrix ───────────────────────────

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
            },
        );

        // The client attempts a divergent self-resize.
        let reported = Rect::new(40, 40, 640, 480);
        assert_ne!(reported, r, "sanity: request differs from the tile");
        let obs = classify_configure(
            reported,
            b,
            &applied.windows[&1],
            engine.state.clients.get(&1).unwrap(),
        );
        assert!(
            matches!(obs, ConfigureObservation::Diverged { follow: false }),
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
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Grid;
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
            },
        );

        let reported = Rect::new(40, 40, 640, 480);
        let obs = classify_configure(
            reported,
            b,
            &applied.windows[&1],
            engine.state.clients.get(&1).unwrap(),
        );
        assert!(
            matches!(obs, ConfigureObservation::Diverged { follow: false }),
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
        };
        let g1 = Rect::new(200, 150, 400, 250);
        let obs = classify_configure(g1, 2, &applied, engine.state.clients.get(&1).unwrap());
        assert!(
            matches!(obs, ConfigureObservation::Diverged { follow: true }),
            "a float's self-resize must be followed, not re-asserted"
        );

        // Simulate adoption: the model adopts the reported rect into client.geom
        // and the Applied entry tracks it; a second classify must be Compliant.
        engine.state.clients.get_mut(&1).unwrap().geom = g1;
        applied = AppliedWindow {
            rect: g1,
            border_w: 2,
            seen: true,
        };
        let obs2 = classify_configure(g1, 2, &applied, engine.state.clients.get(&1).unwrap());
        assert!(
            matches!(obs2, ConfigureObservation::Compliant),
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
        };
        let mut last = g0;
        for i in 0..5 {
            let r = Rect::new(
                100 + (i + 1) * 11,
                100 + (i + 1) * 11,
                300 + (i as u32 + 1) * 20,
                200 + (i as u32 + 1) * 20,
            );
            let obs = classify_configure(r, 2, &applied, engine.state.clients.get(&1).unwrap());
            assert!(
                matches!(obs, ConfigureObservation::Diverged { follow: true }),
                "float request {i} must be followed"
            );
            engine.state.clients.get_mut(&1).unwrap().geom = r;
            applied = AppliedWindow {
                rect: r,
                border_w: 2,
                seen: true,
            };
            last = r;
        }
        let obs = classify_configure(last, 2, &applied, engine.state.clients.get(&1).unwrap());
        assert!(
            matches!(obs, ConfigureObservation::Compliant),
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
        // NOTE: the real X11 handler `events.rs::on_configure_request` float
        // branch is NOT in-memory testable (it performs an X11 round-trip: reads
        // the reported rect, optionally ignores it, and calls configure_window on
        // the server). We therefore test only the PURE MODEL contract: feeding a
        // bogus requested geometry into a floating window's `client.geom` and
        // then running `arrange()` (via `pipeline_desired`) must clamp the float
        // into the monitor workarea and keep `State::check_invariants()` Ok. The
        // X11 event handler itself is deferred to the Xephyr integration phase.
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

    // Fase 1.3 (model-level, tiled): a hostile ConfigureRequest with invalid
    // geometry (0×0, 60000×60000, off-monitor) against a TILED window must be
    // classified `Diverged { follow: false }` AND the WM's own Desired must stay
    // positive — the model never collapses to a degenerate rect, and
    // `client.geom` is never overwritten by the bogus report.
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
            };
            let obs = classify_configure(
                reported,
                engine.cfg.border_w,
                &applied,
                &engine.state.clients[&1],
            );
            assert!(
                matches!(obs, ConfigureObservation::Diverged { follow: false }),
                "tiled invalid ConfigureRequest {reported:?} must be Diverged{{follow:false}}"
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

    // ─── Phase 2 — fullscreen / new windows ──────────────────────────────────

    #[test]
    fn audit_p2_fullscreen_lifecycle_no_orphan_overlay() {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Grid;
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
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Grid;
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
        };

        // Simulate an unexpected ConfigureNotify for A (fullscreen) and B (tiled).
        let obs_a = classify_configure(
            Rect::new(40, 40, 640, 480),
            0,
            &a_applied,
            engine.state.clients.get(&1).unwrap(),
        );
        let obs_b = classify_configure(
            Rect::new(10, 10, 800, 600),
            b_b,
            &b_applied,
            engine.state.clients.get(&2).unwrap(),
        );
        assert!(
            matches!(obs_a, ConfigureObservation::Diverged { follow: false }),
            "A (fullscreen) must NOT follow"
        );
        assert!(
            matches!(obs_b, ConfigureObservation::Diverged { follow: false }),
            "B (tiled) must NOT follow"
        );
        // This proves no "fullscreen == overlay" regression: the WM is the
        // authority for tiled AND fullscreen windows and never follows.

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

    // ─── Phase 3 — maximize / float ──────────────────────────────────────────

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
        aud_run_cmd(&mut engine, crate::core::commands::ToggleFloat);
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
        aud_run_cmd(&mut engine, crate::core::commands::ToggleFloat);
        assert!(engine.state.clients.get(&1).unwrap().is_float());
        // client changes geometry
        let g1 = Rect::new(200, 150, 400, 250);
        engine.state.clients.get_mut(&1).unwrap().geom = g1;
        assert_eq!(engine.state.clients.get(&1).unwrap().geom, g1);

        // back to tiled — geometry authority returns to the WM
        aud_run_cmd(&mut engine, crate::core::commands::ToggleFloat);
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
            &default_registry(),
            crate::core::layout::Phase::Settled,
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

    // ─── Phase 4 — transient / dialogs ───────────────────────────────────────

    #[test]
    fn audit_p4_fullscreen_dialog_steals_focus() {
        use crate::core::commands::decide_manage_focus;
        use crate::types::WinFlags;
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Grid;
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

    // ─── Riesgo 5 — transient-chain depth ────────────────────────────────────
    //
    // `render::MAX_TRANSIENT_DEPTH` (4) bounds the *stacking* question "is this
    // float owned by the presented overlay?" — the bound exists because
    // `WM_TRANSIENT_FOR` is unvalidated client input and can describe a cycle.
    // The bound is a stacking answer only; it must never leak into ownership of
    // the model. These tests build chains at, below and beyond the bound and
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

    /// Root window `1` presenting an overlay (fullscreen in `Grid`, or focused-
    /// maximized) plus a transient chain `1 → 2 → … → depth+1`.
    fn r5_build_chain(depth: u32, maximized: bool) -> Engine {
        let mut engine = setup_engine();
        let mi = engine.state.sel_mon;
        let ws_i = engine.state.monitors[mi].active_ws;
        if !maximized {
            engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Grid;
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
        // The concrete regression behind the fix: destroying the parent used to
        // leave every child's `transient_parent` pointing at a window id that is
        // no longer a client. With XID reuse that stale id can come back as an
        // unrelated window, which would then inherit these orphans as its popups.
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

    // ─── Phase 5 — multi-monitor / multi-workspace ───────────────────────────

    #[test]
    fn audit_p5_monitor_switch_keeps_other_overlay() {
        let mut engine = setup_engine_multi();
        let m0 = 0;
        let m1 = 1;
        engine.state.sel_mon = m0;
        engine.state.monitors[m0].workspaces[0].layout = LayoutKind::Grid;
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
        engine.state.monitors[m0].workspaces[0].layout = LayoutKind::Grid;
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

    // Fase 5 (deterministic): move a window ACROSS monitors, then destroy it.
    // Neither the old monitor nor the new one may retain a Desired/Applied or
    // tree reference to the dead window; `check_invariants` must stay green and
    // no stale `presented_maximize`/`pending_focus` may name it.
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

    // Fixed: the production move/destroy path no longer leaves a stale
    // `presented_maximize` referencing a window that has moved away or been
    // destroyed (see `remove_client` + `MoveWindowToMonitor`/`MoveToWorkspace`).
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
                    engine.execute(ToggleFloat);
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
                        LayoutKind::Grid
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

    // ─── Phase 9 — bug hunt (A–G) ────────────────────────────────────────────

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
            },
        );
        // Set a pending_focus referencing the window (8c context).
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
        engine.state.monitors[mi].workspaces[0].layout = LayoutKind::Grid;
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
            let obs = classify_configure(r, 2, &applied, engine.state.clients.get(&1).unwrap());
            assert!(
                matches!(obs, ConfigureObservation::Diverged { follow: true }),
                "iteration {i}: float fight must be followed"
            );
            engine.state.clients.get_mut(&1).unwrap().geom = r;
            applied = AppliedWindow {
                rect: r,
                border_w: 2,
                seen: true,
            };
            last = r;
            iterations += 1;
        }
        // MEASUREMENT NOTE: the loop completed exactly 200 iterations; every
        // iteration produced a `follow:true` verdict with no backoff mechanism
        // throttling the adoptions (count = 200, all followed).
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

    // ─── AUDITORÍA FASE 6: property harness realista ───────────────────────────
    //
    // A realistic, in-memory property test that fuzzes the FULL client
    // interaction surface (manage / destroy / focus / fullscreen / maximize /
    // float / move-resize / workspace-switch / monitor-switch / ConfigureRequest
    // / ConfigureNotify) and checks invariants after EVERY step. ConfigureX
    // events are simulated at the model/policy level via the reconciler's
    // `classify_configure` — NO X11 connection is opened. The backend's last-
    // written geometry is a single long-lived `AppliedState`; each step merges
    // `pipeline_desired` across both monitors to build the whole-desktop Desired,
    // asserts structural properties, then `reconcile`s and applies the effects.
    // Coverage counters guarantee the run was not vacuous.

    // PHASE 6 property harness — realistic client-resistance fuzz.
    //
    // The column/ribbon scroll model (niri-style) deliberately scrolls
    // NON-FOCUSED columns partially or fully off-screen; the compositor clips
    // them per monitor. `State::check_invariants` (src/types.rs:1476) does NOT
    // assert geometry-positivity or on-screen bounds — those hold only after a
    // placement pass and many valid transient states have off-screen rects. So
    // off-screen Desired rects are BY DESIGN, not a bug (and even the focused
    // window can be off-screen transiently while the camera spring is mid-
    // animation, so the harness does NOT assert focused-within-screen either).
    // This harness therefore only checks FINITE + positive rects for every
    // Desired window (rect coords are i32, so "finite" is inherent — this is a
    // non-positive-size guard), plus that every Desired window id exists in
    // `state.clients` and that the `raise` list references known windows.
    // Full check_invariants runs every step; reconcile convergence,
    // classify_configure policy, and the destroy-before-reconcile race are all
    // exercised.
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
                all.raise.extend(d.raise);
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

        // Mirror the backend's focus sink: commands that emit `FocusWindow`
        // actually move `mon.focused` (the core command only *emits*). The real
        // `Backend::focus()` focuses on the window's OWN monitor (`mon_i =
        // c.monitor`) AND sets `sel_mon = mon_i` (render.rs:746, 798-799), so a
        // focus on a window living on another monitor keeps `sel_mon` consistent
        // with the focused window's monitor. Mirror both here: setting
        // `monitors[sel_mon].focused` alone would desync them and let a later
        // `ToggleFloat`/`ToggleFullscreen` act on `sel_mon` and re-insert the
        // window into the wrong monitor's tree (a false cross-monitor duplicate).
        macro_rules! run {
            ($cmd:expr) => {{
                // Mirror the real backend's `Backend::focus()`: the engine always
                // acts on `sel_mon`, so `sel_mon` must name the monitor that
                // actually contains the focused window. A desync here (the
                // focused window living on a *different* monitor) would make a
                // sel_mon-based command (`ToggleFloat`/`ToggleMaximize`/
                // `ToggleFullscreen`/`MoveResize` all remove from and re-insert
                // into `monitors[sel_mon]`) tear the window out of its true tree
                // and re-insert it on the wrong monitor — a false cross-monitor
                // duplicate that has nothing to do with the WM core. The
                // production backend never desyncs because `focus()` sets
                // `mon_i = c.monitor` AND `sel_mon = mon_i`; the harness must do
                // the same before every command it drives.
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
                                    && (ws.layout == LayoutKind::Grid || c.is_true_fullscreen()))
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
                    run!(ToggleFloat);
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
                //    authority (Diverged{follow:false}); a pure float ⇒ follow.
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
                            let obs = classify_configure(
                                reported,
                                a.border_w,
                                &a,
                                engine.state.clients.get(&w).unwrap(),
                            );
                            let ok = matches!(obs, ConfigureObservation::Diverged { follow: f } if f == is_float_fs);
                            assert!(
                                ok,
                                "seed {seed:#x} step {step} op ConfigureRequest win {w}: expected Diverged{{follow:{is_float_fs}}} but got a different verdict",
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
                                let obs = classify_configure(
                                    reported,
                                    a.border_w,
                                    &a,
                                    engine.state.clients.get(&w).unwrap(),
                                );
                                match obs {
                                    ConfigureObservation::Compliant => {}
                                    ConfigureObservation::Diverged { follow } => assert_eq!(
                                        follow,
                                        is_float_fs,
                                        "seed {seed:#x} step {step} op ConfigureNotify win {w}: follow {follow} != expected {is_float_fs}"
                                    ),
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
                    // `retarget_focus_to_window` centers the camera via `ideal_scroll`,
                    // which uses the LIVE accordion boost. The harness never ticks the
                    // boost animation, so the live boost is stale; `pipeline_desired`
                    // projects with `Phase::Settled` (boost forced to its rest value),
                    // and a camera centered on the stale live widths drifts the focused
                    // column off-screen. Re-center the camera on the SAME settled
                    // geometry so the focused column is on-screen (the real compositor
                    // eases the boost to rest, so Live==Settled there too).
                    let aws = engine.state.monitors[mi].active_ws;
                    let scroll = {
                        let m = &engine.state.monitors[mi];
                        let ws = &m.workspaces[aws];
                        let fs = crate::core::layout::fs_ctx(&engine.state.clients, ws, m.screen);
                        let g = crate::core::layout::ribbon_geom(
                            ws,
                            &engine.cfg,
                            m.workarea,
                            true,
                            &fs,
                        );
                        if g.cols.is_empty() {
                            0.0f32
                        } else {
                            let i = ws.focus.column_idx.min(g.cols.len() - 1);
                            let (cx0, cw) = g.cols[i];
                            let waw = g.wa.w as f32;
                            let cam_min = g.cx / g.alpha;
                            let cam_max = g.total_w - (waw - g.cx) / g.alpha;
                            if fs.cols.contains(&i) && ws.layout == crate::types::LayoutKind::Column
                            {
                                let cam =
                                    cx0 + (g.wa.x as f32 + g.cx - fs.screen.x as f32) / g.alpha;
                                if cam_max <= cam_min {
                                    (g.total_w - waw) / 2.0
                                } else {
                                    cam.clamp(cam_min, cam_max)
                                }
                            } else {
                                let want = cx0 + cw / 2.0 - waw / 2.0;
                                if cam_max <= cam_min {
                                    (g.total_w - waw) / 2.0
                                } else {
                                    want.clamp(cam_min, cam_max)
                                }
                            }
                        }
                    };
                    let m = &mut engine.state.monitors[mi];
                    m.workspaces[aws].camera.target = scroll;
                    m.workspaces[aws].camera.position = scroll;
                }
                let aws = engine.state.monitors[mi].active_ws;
                let target = engine.state.monitors[mi].workspaces[aws].camera.target;
                engine.state.monitors[mi].workspaces[aws].camera.position = target;
            }

            // Build the whole-desktop Desired for this step.
            let desired = run_pipeline_all(&engine);

            // Directed Desired assertions (WEAK — off-screen Desired rects are by
            // design; the ribbon/column scroll model places non-focused columns
            // off-screen and the camera spring can transiently hold even the
            // focused window off-screen, so we do NOT assert on-screen bounds):
            //  - every Desired window id exists in state.clients
            //  - every Desired rect is finite + positive (real corruption only;
            //    Rect coords are i32 so NaN/inf cannot occur — this is just a
            //    non-positive-size guard)
            //  - the raise list references only known windows
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
            for &rw in &desired.raise {
                assert!(
                    engine.state.clients.contains_key(&rw),
                    "seed {seed:#x} step {step}: raise references unknown window {rw}"
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
                // Floats must follow the model (client.geom); overlays are
                // excluded (the WM is authoritative for them).
                if let Some(c) = engine.state.clients.get(win) {
                    if c.is_float() && !c.is_fullscreen() {
                        let mon = &engine.state.monitors[c.monitor];
                        let ws = &mon.workspaces[c.workspace];
                        let is_overlay = (c.is_fullscreen()
                            && (ws.layout == LayoutKind::Grid || c.is_true_fullscreen()))
                            || ws.presented_maximize == Some(*win);
                        if !is_overlay {
                            let wa = mon.workarea;
                            let g = c.geom;
                            let fits = g.x >= wa.x
                                && g.y >= wa.y
                                && g.x + g.w as i32 <= wa.x + wa.w as i32
                                && g.y + g.h as i32 <= wa.y + wa.h as i32;
                            if fits {
                                assert_eq!(
                                        drect, g,
                                        "seed {seed:#x} step {step}: float {win} desired must equal client.geom"
                                    );
                            } else {
                                assert!(
                                        drect.w <= wa.w && drect.h <= wa.h,
                                        "seed {seed:#x} step {step}: float {win} clamped rect {drect:?} exceeds workarea {wa:?}"
                                    );
                            }
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

            // Keep the focus deferral consistent with the backend's teardown
            // policy: if the owning overlay is gone, consume the deferral.
            if let Some(pf) = engine.state.pending_focus {
                let owner_presented = engine.state.monitors.get(pf.monitor).is_some_and(|m| {
                    let focused = m.focused;
                    m.workspaces.get(pf.workspace).is_some_and(|ws| {
                        engine.state.clients.get(&pf.owner).is_some_and(|c| {
                            c.monitor == pf.monitor
                                && c.workspace == pf.workspace
                                && ((c.is_fullscreen()
                                    && (ws.layout == LayoutKind::Grid || c.is_true_fullscreen()))
                                    || ((c.is_maximized_v() || c.is_maximized_h())
                                        && focused == Some(pf.owner)))
                        })
                    })
                });
                if !owner_presented {
                    consume_pending_focus(&mut engine.state, pf.monitor, pf.workspace, None);
                }
            }

            // The structural manifesto: full invariants after EVERY step.
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
            0x600D_F00D_CAFE_BABE,
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

    // ─── Riesgo-3: model A fullscreen ConfigureRequest is ignored / WM reasserts Desired ─
    #[test]
    fn configure_request_fullscreen_is_ignored_model_a() {
        use crate::backend::x11::reconciler::{
            classify_configure, AppliedWindow, ConfigureObservation,
        };
        use crate::core::commands::ToggleFullscreen;
        use crate::types::{Action, LayoutKind, Rect, WindowId};

        let mut engine = setup_engine();
        // Model A: the fullscreen ConfigureRequest path in `on_configure_request`
        // returns early without adopting the client rect — the WM reasserts its own
        // Desired. Force a Grid layout so a fullscreen window becomes a presented
        // overlay owner (the model-A branch).
        engine.dispatch(Action::SetLayout(LayoutKind::Grid));

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
        };
        let obs = classify_configure(
            reported,
            engine.cfg.border_w,
            &applied,
            &engine.state.clients[&w],
        );
        assert!(
            matches!(obs, ConfigureObservation::Diverged { follow: false }),
            "model A: a fullscreen ConfigureRequest must be Diverged{{follow:false}} (WM authority)"
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

    // ─── EWMH `_NET_ACTIVE_WINDOW` focus-theft policy ──────────────────────────
    //
    // `decide_active_window` is the pure policy the X11 handler calls for every
    // `_NET_ACTIVE_WINDOW` request. It must refuse to let an unrelated window
    // steal focus from a presented fullscreen/maximize overlay on the *same*
    // (monitor, workspace) as the requester, while still honoring the overlay
    // owner itself and any dialog it owns.

    /// Register + tile `win` on an explicit (monitor, workspace), optionally
    /// making it a Grid fullscreen overlay owner there.
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
            engine.state.monitors[mi].workspaces[ws_i].layout = LayoutKind::Grid;
            let mon = &mut engine.state.monitors[mi];
            mon.focus_stack.retain(|&w| w != win);
            mon.focus_stack.push(win);
            engine
                .state
                .clients
                .get_mut(&win)
                .unwrap()
                .flags
                .set(WinFlags::FULLSCREEN);
        }
    }

    #[test]
    fn net_active_window_respects_presented_overlay_policy() {
        use crate::core::commands::{decide_active_window, ActiveWindowIntent};

        // 1) Plain tiled B cannot steal focus from a Grid fullscreen overlay A on
        //    the same (mon0, ws0).
        {
            let mut engine = setup_engine();
            let mi = engine.state.sel_mon;
            let ws_i = engine.state.monitors[mi].active_ws;
            aw_add_client(&mut engine, 1, mi, ws_i, true); // A: Grid fullscreen overlay
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
}
