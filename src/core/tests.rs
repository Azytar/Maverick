#[cfg(test)]
mod unit_tests {
    use crate::config::Cfg;
    use crate::core::Engine;
    use crate::types::{Action, LayoutKind, Monitor, Rect};

    // 1. Extract config into a helper to keep each test clean.
    // Ideally Cfg would implement `Default` in the future.
    fn default_cfg() -> Cfg {
        Cfg {
            border_w: 2,
            gaps: 6,
            n_tags: 9,
            default_col_w: 700,
            split_bias: 0.6,
            focus_mouse: false,
            warp_cursor: false,
            col_normal: 0,
            col_focused: 0,
            col_urgent: 0,
            tag_names: vec!["1", "2", "3", "4", "5", "6", "7", "8", "9"],
            keybinds: vec![],
            rules: vec![],
            compositor: vec![],
            compositor_delay_ms: 0,
            startup_sound: None,
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
        arrange(&engine.state, mi, &engine.cfg, &mut placements);

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
}
