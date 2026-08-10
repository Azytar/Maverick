#[cfg(test)]
mod unit_tests {
    use crate::config::Cfg;
    use crate::core::layout::{FsCtx, LayoutRegistry, RibbonScratch};
    use crate::core::Engine;
    use crate::types::{Action, LayoutKind, Monitor, Rect};

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
        arrange(            &engine.state,
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
        arrange(            &engine.state,
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
        arrange(            &engine.state,
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
        arrange(            &engine.state,
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
        arrange(            &engine.state,
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
        crate::core::layout::arrange(            &engine.state,
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
        crate::core::layout::arrange(            &engine.state,
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
                arrange(                    &engine.state,
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
                arrange(                    &engine.state,
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
                arrange(                    &engine.state,
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
                    let (_, geom, bw) = placements[i]; // placements are pushed in column order
                    let pl = geom.x as f32;
                    let pr = (geom.x + geom.w as i32 + 2 * bw as i32) as f32;
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
        arrange(            &engine.state,
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
        arrange(            &engine.state,
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
        assert_eq!(engine.state.best_focus(mi), Some(1));
        engine.state.monitors[mi].focused = Some(2);

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
        arrange(            &engine.state,
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
    // float. The core command owns this topology change; the FULLSCREEN flag itself
    // is owned by the backend's `set_fullscreen` handler.

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

        // Simulate the backend applying the FULLSCREEN flag (owned by set_fullscreen).
        engine
            .state
            .clients
            .get_mut(&win)
            .unwrap()
            .flags
            .set(WinFlags::FULLSCREEN);

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

    // ─── Fase 1: the EWMH fullscreen path must promote a float too (bug C1/A1) ────
    //
    // The keyboard path (`ToggleFullscreen`) always promoted a float into the
    // tiling before the backend set the `FULLSCREEN` flag. The EWMH path
    // (`_NET_WM_STATE_FULLSCREEN` client message → `set_fullscreen`) did not, so a
    // float — mpv is the canonical case — stayed in `ws.floats`, kept being laid
    // out from `client.geom`, and the old `Rect::default()` sentinel collapsed it
    // to 0×0. `apply_fullscreen_topology` is now the one shared implementation
    // both paths call.

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

        // The backend's `set_fullscreen` runs this before flipping the flag.
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
        arrange(            &engine.state,
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

        // The keyboard path runs it once inside the command; the effect handler
        // runs it again. The second call must change nothing.
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
}
