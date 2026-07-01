#[cfg(test)]
mod unit_tests {
    use crate::config::Cfg;
    use crate::core::{AppEvent, Command, Engine};
    use crate::types::{Action, LayoutKind, Monitor, Rect};

    // 1. Extract config into a helper to keep each test clean.
    // Ideally Cfg would implement `Default` in the future.
    fn default_cfg() -> Cfg {
        Cfg {
            border_w: 2,
            gaps: 6,
            bar_height: 22,
            top_bar: true,
            n_tags: 9,
            default_col_w: 700,
            split_bias: 0.6,
            focus_mouse: false,
            warp_cursor: false,
            col_normal: 0,
            col_focused: 0,
            col_urgent: 0,
            col_bar_bg: 0,
            col_bar_fg: 0,
            col_bar_sel: 0,
            col_bar_occ: 0,
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
            .push(Monitor::new(Rect::new(0, 0, 1920, 1080), 22, true, 9));
        engine
    }

    #[test]
    fn test_toggle_bar_hides_and_shows() {
        let mut engine = setup_engine();
        assert!(
            engine.state.monitors[0].show_bar,
            "bar should start visible"
        );

        // Action 1: Hide the bar
        let cmds_hide = engine.process_event(AppEvent::ActionTriggered(Action::ToggleBar));
        assert!(
            !engine.state.monitors[0].show_bar,
            "bar not hidden in state"
        );
        assert!(
            cmds_hide
                .iter()
                .any(|cmd| matches!(cmd, Command::UpdateBar(_))),
            "missing command to tell backend to redraw the bar"
        );

        // Action 2: Show the bar again
        let cmds_show = engine.process_event(AppEvent::ActionTriggered(Action::ToggleBar));
        assert!(engine.state.monitors[0].show_bar, "bar was not shown again");
        assert!(
            cmds_show
                .iter()
                .any(|cmd| matches!(cmd, Command::UpdateBar(_))),
            "missing redraw command on the second pass"
        );
    }

    #[test]
    fn test_cycle_layout_wraps_around() {
        let mut engine = setup_engine();

        assert_eq!(
            engine.state.monitors[0].workspaces[0].layout,
            LayoutKind::Column
        );

        engine.process_event(AppEvent::ActionTriggered(Action::CycleLayout));
        assert_eq!(
            engine.state.monitors[0].workspaces[0].layout,
            LayoutKind::Monocle
        );

        engine.process_event(AppEvent::ActionTriggered(Action::CycleLayout));
        assert_eq!(
            engine.state.monitors[0].workspaces[0].layout,
            LayoutKind::Grid
        );

        engine.process_event(AppEvent::ActionTriggered(Action::CycleLayout));
        assert_eq!(
            engine.state.monitors[0].workspaces[0].layout,
            LayoutKind::Column,
            "layout cycle must wrap Column→Monocle→Grid→Column",
        );
    }

    #[test]
    fn test_window_created_emits_layout_commands() {
        let mut engine = setup_engine();
        let new_window_id = 1001;

        // Simulate the backend capturing a MapRequest and forwarding it to the core
        let event = AppEvent::WindowCreated(new_window_id);
        let commands = engine.process_event(event);

        // Verify the WM computed the layout math and
        // emitted the physical command to move the window to its coordinates.
        let has_move_resize = commands
            .iter()
            .any(|cmd| matches!(cmd, Command::MoveResize { win, .. } if *win == new_window_id));

        assert!(
            has_move_resize,
            "creating a window must trigger layout computation and emit MoveResize"
        );
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
        ws.focus = Focus {
            column_idx: 0,
            window_idx: 0,
        };
        engine.state.monitors[0].focused = Some(10);
        engine
    }

    #[test]
    fn test_move_right_single_window_swaps_not_merges() {
        let mut engine = setup_two_columns();
        engine.state.apply_move_dir(crate::types::Dir::Right, 700);
        let ws = &engine.state.monitors[0].workspaces[0];
        assert_eq!(ws.columns.len(), 2, "swap must keep 2 separate columns");
        assert_eq!(ws.columns[0].windows, vec![20]);
        assert_eq!(ws.columns[1].windows, vec![10]);
        assert_eq!(ws.focus.column_idx, 1);
    }

    #[test]
    fn test_move_left_right_reversible() {
        let mut engine = setup_two_columns();
        engine.state.apply_move_dir(crate::types::Dir::Right, 700);
        engine.state.apply_move_dir(crate::types::Dir::Left, 700);
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
        ws.focus = Focus {
            column_idx: 0,
            window_idx: 0,
        };
        engine.state.monitors[0].focused = Some(10);

        engine.state.apply_move_dir(crate::types::Dir::Right, 700);
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
        ws.focus = Focus {
            column_idx: 0,
            window_idx: 0,
        };
        engine.state.monitors[0].focused = Some(10);

        let changed = engine.state.apply_move_dir(crate::types::Dir::Right, 700);
        assert!(!changed, "move at rightmost boundary must return false");
        assert_eq!(engine.state.monitors[0].workspaces[0].columns.len(), 1);
    }
}
