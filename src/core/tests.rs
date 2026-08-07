#[cfg(test)]
mod unit_tests {
    use crate::config::Cfg;
    use crate::core::Engine;
    use crate::core::layout::LayoutRegistry;
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
            default_col_w: 700,
            split_bias: 0.6,
            focus_mouse: false,
            warp_cursor: false,
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
        let workarea_w = engine.state.monitors[mi].workarea.w;
        engine.state.monitors[mi].workspaces[ws_i].add_tiled(
            new_window_id,
            engine.cfg.default_col_w,
            workarea_w,
        );
        let mut client = Client::new(new_window_id, mi, ws_i);
        client.border_w = engine.cfg.border_w;
        engine.state.add_client(client);

        // Run the pure layout the live path uses (backend::arrange → layout::arrange).
        let mut placements = Placements::with_capacity(4);
        let registry = default_registry();
        arrange(&engine.state, mi, &engine.cfg, &registry, &mut placements);

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
            width: 600,
        });
        ws.columns.push(Column {
            windows: vec![20],
            focused: 0,
            width: 600,
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
            width: 800,
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
            width: 600,
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
                width: 600,
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
                width: 600,
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
                width: 600,
            });
            ws.focus = Focus { column_idx: 0 };
        }
        engine.state.monitors[0].focused = Some(42);
        engine
            .state
            .clients
            .insert(7, Client::new(7, 0, 0));
        engine
            .state
            .clients
            .insert(42, Client::new(42, 0, 0));
        engine
            .state
            .clients
            .get_mut(&7)
            .unwrap()
            .flags
            .set(WinFlags::FULLSCREEN);
        // Focus history: 42 was peeked most recently, 7 was fullscreen before.
        engine.state.monitors[0].focus_stack = vec![7, 42];

        // Close 42 (the peeked tile) → 7 is still fullscreen →
        // best_focus must return 7, not a stale in-visible mosaic.
        assert_eq!(engine.state.best_focus(0), Some(7));

        // Without any overlay window, behavior stays column-focused.
        // Without any overlay window, behavior stays column-focused.
        engine.state.clients.get_mut(&7).unwrap().flags.clear(WinFlags::FULLSCREEN);
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
        engine.state.clients.insert(9, crate::types::Client::new(9, 0, 0));
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
}




